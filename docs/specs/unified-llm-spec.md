# Unified LLM Client Specification

This document is a consolidated, language-agnostic specification for building a unified client library that provides a single interface across multiple LLM providers (OpenAI, Anthropic, Google Gemini, and others). It is designed to be implementable from scratch by any developer or coding agent in any programming language.

---

## Table of Contents

1. [Overview and Goals](#1-overview-and-goals)
2. [Architecture](#2-architecture)
3. [Data Model](#3-data-model)
4. [Generation and Streaming](#4-generation-and-streaming)
5. [Tool Calling](#5-tool-calling)
6. [Error Handling and Retry](#6-error-handling-and-retry)
7. [Provider Adapter Contract](#7-provider-adapter-contract)
8. [Definition of Done](#8-definition-of-done)

---

## 1. Overview and Goals

### 1.1 Problem Statement

Applications that use large language models face a fragmented ecosystem. Each provider -- OpenAI, Anthropic, Google Gemini, and others -- exposes a different HTTP API with different message formats, tool calling conventions, streaming protocols, error shapes, and authentication mechanisms. Switching providers or supporting multiple providers requires rewriting request construction, response parsing, error handling, and streaming logic.

This specification defines a unified client library that solves this problem. Developers write provider-agnostic code and switch models by changing a single string identifier. No rewiring, no adapter-specific imports.

### 1.2 Design Principles

**Provider-agnostic.** Application code should not contain provider-specific logic. The unified interface handles all translation. Provider-specific features are available through an explicit escape hatch, not through leaky abstractions.

**Minimal surface area.** The library exposes a small number of types and functions. A developer can learn the full API in under an hour. Fewer concepts means fewer bugs and easier maintenance.

**Streaming-first.** Streaming is a first-class operation, not a flag on a blocking call. The two generation modes -- blocking and streaming -- have separate methods with distinct return types. This makes the type system work for the developer.

**Composable.** Cross-cutting concerns (logging, retries, caching) are handled through middleware, not baked into the core. The core client is a thin routing layer.

**Escape hatches over false abstractions.** When a provider offers a unique feature that does not map to the unified model, the library provides a pass-through mechanism rather than pretending the feature does not exist or building an unreliable shim.

### 1.3 Reference Open-Source Projects

The following open-source projects solve related problems and are worth studying for patterns, trade-offs, and lessons learned. They are not dependencies; implementors may take inspiration from any combination of them.

- **Vercel AI SDK** (https://github.com/vercel/ai) -- TypeScript. Multi-provider architecture with a versioned provider specification. Clean separation between provider interfaces and high-level convenience API (`generateText`/`streamText`/`generateObject`). Demonstrates the start/delta/end streaming event pattern and a composable middleware system.

- **LiteLLM** (https://github.com/BerriAI/litellm) -- Python. Supports 100+ providers behind a single `completion()` interface. Demonstrates the value of a unified calling convention and the model string routing pattern. Shows how to handle the long tail of provider-specific quirks at scale.

- **pi-ai** (https://github.com/nicktmro/pipe-ai) -- TypeScript. A multi-provider AI client from @mariozechner's pi-mono project. Demonstrates cost tracking, usage aggregation, and a clean provider adapter pattern with explicit reasoning token support.

---

## 2. Architecture

### 2.1 Four-Layer Architecture

The library is organized into four layers, each with a clear responsibility boundary.

```
Layer 4: High-Level API         generate(), stream(), generate_object()
          ---------------------------------------------------------------
Layer 3: Core Client            Client, provider routing, middleware hooks
          ---------------------------------------------------------------
Layer 2: Provider Utilities     Shared helpers for building adapters
          ---------------------------------------------------------------
Layer 1: Provider Specification  ProviderAdapter interface, shared types
```

**Layer 1 -- Provider Specification.** Defines the contract that every provider adapter must implement. Contains only interface definitions and shared type definitions. No implementation logic. This layer is the stability contract: it changes rarely and only with explicit versioning. A new provider is added by implementing this interface, not by modifying it.

**Layer 2 -- Provider Utilities.** Contains shared code for building adapters: HTTP client helpers, Server-Sent Events (SSE) parsing, retry logic, response normalization utilities, JSON schema translation helpers. Provider adapter authors import this layer; application developers generally do not.

**Layer 3 -- Core Client.** The main orchestration layer. The `Client` object holds registered provider adapters, routes requests by provider identifier, applies middleware, and manages configuration. This is the primary import for application code that wants direct control over requests.

**Layer 4 -- High-Level API.** Provides convenience functions (`generate()`, `stream()`, `generate_object()`) that wrap the Client with ergonomic defaults. Most application code uses this layer. These functions handle prompt standardization, tool execution loops, output parsing, structured output validation, and automatic retries.

### 2.2 Client Configuration

#### Environment-Based Setup

The recommended setup for most applications reads standard environment variables per provider:

```
client = Client.from_env()
```

Environment variable conventions:

| Provider  | Required Variable      | Optional Variables                                  |
|-----------|------------------------|-----------------------------------------------------|
| OpenAI    | OPENAI_API_KEY         | OPENAI_BASE_URL, OPENAI_ORG_ID, OPENAI_PROJECT_ID  |
| Anthropic | ANTHROPIC_API_KEY      | ANTHROPIC_BASE_URL                                  |
| Gemini    | GEMINI_API_KEY         | GEMINI_BASE_URL                                     |

Alternate key names may be accepted (e.g., `GOOGLE_API_KEY` as a fallback for `GEMINI_API_KEY`). Only providers whose keys are present in the environment are registered. The first registered provider becomes the default.

#### Programmatic Setup

For full control, adapters are constructed explicitly and registered with the Client:

```
adapter = OpenAIAdapter(
    api_key = "sk-...",
    base_url = "https://custom-endpoint.example.com/v1",
    default_headers = { "X-Custom": "value" },
    timeout = 30.0
)

client = Client(
    providers = { "openai": adapter },
    default_provider = "openai"
)
```

#### Provider Resolution

When a request specifies a `provider` field, the Client routes to that adapter. When the provider field is omitted, the Client uses `default_provider`. If no default is set and no provider is specified, the Client raises a configuration error. The Client never guesses.

#### Model String Convention

Model identifiers are the provider's native string (e.g., `"gpt-5.2"`, `"claude-opus-4-6"`, `"gemini-3-flash-preview"`). The library does not invent its own model namespace. This avoids the maintenance burden of mapping tables and ensures new models work immediately without library updates. If a model string could be ambiguous (multiple providers support it), the `provider` field on the request disambiguates.

### 2.3 Middleware / Interceptor Pattern

The Client supports middleware for cross-cutting concerns. Middleware wraps provider calls and can inspect or modify requests, inspect or modify responses, and perform side effects.

```
FUNCTION logging_middleware(request, next):
    LOG("Request to " + request.provider + "/" + request.model)
    response = next(request)
    LOG("Response: " + response.usage.total_tokens + " tokens")
    RETURN response

client = Client(
    providers = { ... },
    middleware = [logging_middleware]
)
```

**Execution order.** Middleware runs in registration order for the request phase (first registered = first to execute) and in reverse order for the response phase. This is the standard onion/chain-of-responsibility pattern.

**Streaming middleware.** Middleware must also apply to streaming requests. For streaming, middleware wraps the event iterator and can observe or transform individual stream events. The middleware interface should support both modes:

```
FUNCTION streaming_middleware(request, next):
    event_iterator = next(request)
    FOR EACH event IN event_iterator:
        log_event(event)
        YIELD event
```

**Common middleware use cases:**
- Logging
- Request/response caching
- Cost tracking and budgets
- Client-side rate limiting
- Prompt injection detection
- Circuit breaker pattern

### 2.4 Provider Adapter Interface

Every provider must implement this interface:

```
INTERFACE ProviderAdapter:
    PROPERTY name : String             -- e.g., "openai", "anthropic", "gemini"

    FUNCTION complete(request: Request) -> Response
        -- Send a request, block until the model finishes, return the full response.

    FUNCTION stream(request: Request) -> AsyncIterator<StreamEvent>
        -- Send a request, return an asynchronous iterator of stream events.
```

**Why two methods, not one.** A single method with a `stream: boolean` flag was rejected because the return types are fundamentally different. A blocking `Response` and an asynchronous event stream have different consumption patterns, error handling models, and lifetime semantics. Separate methods make the type system work for the developer.

**No separate `send_tool_outputs` method.** Tool results are sent by including them in the message history of a new `complete()` or `stream()` call. This matches how Anthropic and Gemini work natively. The OpenAI adapter handles any translation internally.

#### Optional Adapter Methods

These methods are recommended but not required:

```
FUNCTION close() -> Void
    -- Release resources (HTTP connections, etc.). Called by Client.close().

FUNCTION initialize() -> Void
    -- Validate configuration on startup. Called by Client on registration.

FUNCTION supports_tool_choice(mode: String) -> Boolean
    -- Query whether a particular tool choice mode is supported.
```

### 2.5 Module-Level Default Client

High-level functions (`generate()`, `stream()`, etc.) use a module-level default client. This client is lazily initialized from environment variables on first use. Applications can override it:

```
set_default_client(my_client)

-- Or pass explicitly per call:
result = generate(model = "...", prompt = "...", client = my_client)
```

### 2.6 Concurrency Model

The library is async-first. All provider calls are non-blocking. The `complete()` and `stream()` methods are asynchronous. The high-level API provides both async and sync wrappers for languages that support both paradigms.

Multiple concurrent requests to different providers (or the same provider) are safe. The Client holds no mutable state between requests. Provider adapters manage their own connection pools and must be safe for concurrent use.

### 2.7 Native API Usage (Critical)

Each provider adapter MUST use the provider's native, preferred API -- not a compatibility layer. This is a fundamental design requirement. Using a lowest-common-denominator compatibility layer (such as only targeting the OpenAI Chat Completions API shape) loses access to provider-specific capabilities like reasoning tokens, extended thinking, prompt caching, and advanced tool features.

| Provider  | Required API                    | Why Not Compatibility Layer                                                |
|-----------|---------------------------------|---------------------------------------------------------------------------|
| OpenAI    | **Responses API** (`/v1/responses`) | The Responses API properly surfaces reasoning tokens, supports built-in tools (web search, file search, code interpreter), and is OpenAI's forward-looking API. The Chat Completions API does not return reasoning tokens for reasoning models (GPT-5.2 series, etc.) and lacks server-side conversation state. |
| Anthropic | **Messages API** (`/v1/messages`)   | The Messages API supports extended thinking with thinking blocks and signatures, prompt caching with `cache_control`, beta feature headers, and the strict user/assistant alternation model. There is no alternative. |
| Gemini    | **Gemini API** (`/v1beta/models/*/generateContent`) | The native Gemini API supports grounding with Google Search, code execution, system instructions, and cached content. OpenAI-compatible endpoints for Gemini are limited shims. |

The unified SDK abstracts over these different APIs so that callers write provider-agnostic code, but internally each adapter speaks the provider's native protocol. This is the entire value proposition: the complexity of three different APIs is handled once in the adapters so that downstream consumers (like a coding agent) never have to think about it.

### 2.8 Provider Beta Headers and Feature Flags

Providers frequently gate new features behind beta headers or feature flags. The unified SDK must support passing these through cleanly.

**Anthropic beta headers.** Anthropic uses the `anthropic-beta` header to enable features like:
- `max-tokens-3-5-sonnet-2025-04-14` -- enables 1M token context for certain models
- `interleaved-thinking-2025-05-14` -- enables interleaved thinking blocks
- `token-efficient-tools-2025-02-19` -- more efficient tool token usage
- `prompt-caching-2024-07-31` -- enables prompt caching

These must be passed as HTTP headers on the request. The adapter should accept them via `provider_options`:

```
request = Request(
    model = "claude-opus-4-6",
    messages = [ ... ],
    provider_options = {
        "anthropic": {
            "beta_headers": ["interleaved-thinking-2025-05-14"]
        }
    }
)
```

The Anthropic adapter joins these into a comma-separated `anthropic-beta` header value.

**OpenAI feature flags.** The Responses API supports enabling built-in tools and features via the request body (e.g., `tools: [{"type": "web_search_preview"}]`). These should be supported through `provider_options` or by extending the tool definitions.

**Gemini configuration.** Gemini supports safety settings, grounding configuration, and cached content references as part of the request body. These should be passable through `provider_options`.

The key principle: the unified interface handles the common 90% of cases. The `provider_options` escape hatch handles the remaining 10% without requiring library changes for every new provider feature.

### 2.9 Model Catalog

The SDK should ship with a catalog of known models to help consumers (especially AI coding agents) select valid model identifiers without guessing or hallucinating model names. The catalog is advisory, not restrictive -- unknown model strings are still passed through to the provider.

```
RECORD ModelInfo:
    id              : String            -- the model's API identifier (e.g., "claude-opus-4-6")
    provider        : String            -- which provider serves this model
    display_name    : String            -- human-readable name (e.g., "Claude Opus 4.6")
    context_window  : Integer           -- maximum total tokens (input + output)
    max_output      : Integer | None    -- maximum output tokens
    supports_tools  : Boolean           -- whether the model supports tool calling
    supports_vision : Boolean           -- whether the model accepts image inputs
    supports_reasoning : Boolean        -- whether the model produces reasoning tokens
    input_cost_per_million  : Float | None  -- cost per 1M input tokens (USD)
    output_cost_per_million : Float | None  -- cost per 1M output tokens (USD)
    aliases         : List<String>      -- shorthand names (e.g., ["sonnet", "claude-sonnet"])
```

**At the time of writing (February 2026),** the top models available through each provider's API are:

| Provider  | Top Model(s)                                        |
|-----------|-----------------------------------------------------|
| Anthropic | **Claude Opus 4.6**, Claude Sonnet 4.5               |
| OpenAI    | **GPT-5.2 series** (GPT-5.2, GPT-5.2-codex)        |
| Gemini    | **Gemini 3 Pro (Preview)**, Gemini 3 Flash (Preview) |

Implementations should default to the latest available models when no model is specified by the caller, and should prefer newer models in any model selection logic. However, the catalog must also include older models that are still served by the APIs, as callers may need them for cost, latency, or compatibility reasons.

Example catalog (keep this updated as new models release):

```
MODELS = [
    -- ==========================================================
    -- Anthropic -- prefer Claude Opus 4.6 for top quality
    -- ==========================================================

    ModelInfo(id="claude-opus-4-6",               provider="anthropic", display_name="Claude Opus 4.6",   context_window=200000, supports_tools=true, supports_vision=true, supports_reasoning=true),
    ModelInfo(id="claude-sonnet-4-5",             provider="anthropic", display_name="Claude Sonnet 4.5", context_window=200000, supports_tools=true, supports_vision=true, supports_reasoning=true),

    -- ==========================================================
    -- OpenAI -- prefer GPT-5.2 series for top quality
    -- ==========================================================

    ModelInfo(id="gpt-5.2",                       provider="openai",    display_name="GPT-5.2",           context_window=1047576, supports_tools=true, supports_vision=true, supports_reasoning=true),
    ModelInfo(id="gpt-5.2-mini",                  provider="openai",    display_name="GPT-5.2 Mini",      context_window=1047576, supports_tools=true, supports_vision=true, supports_reasoning=true),
    ModelInfo(id="gpt-5.2-codex",                 provider="openai",    display_name="GPT-5.2 Codex",     context_window=1047576, supports_tools=true, supports_vision=true, supports_reasoning=true),

    -- ==========================================================
    -- Gemini -- prefer Gemini 3 Flash Preview for latest
    -- ==========================================================

    ModelInfo(id="gemini-3-pro-preview",          provider="gemini",    display_name="Gemini 3 Pro (Preview)",   context_window=1048576, supports_tools=true, supports_vision=true, supports_reasoning=true),
    ModelInfo(id="gemini-3-flash-preview",        provider="gemini",    display_name="Gemini 3 Flash (Preview)", context_window=1048576, supports_tools=true, supports_vision=true, supports_reasoning=true),
]
```

**Lookup functions:**

```
get_model_info(model_id: String) -> ModelInfo | None
    -- Returns the catalog entry for a model, or None if unknown.

list_models(provider: String | None) -> List<ModelInfo>
    -- Returns all known models, optionally filtered by provider.

get_latest_model(provider: String, capability: String | None) -> ModelInfo | None
    -- Returns the newest/best model for a provider, optionally filtered by capability
    -- (e.g., "reasoning", "vision", "tools"). Useful for coding agents that want
    -- to always use the latest available model.
```

**Why a catalog matters for coding agents:** When an AI coding agent builds on top of this SDK, it needs to select models by capability (e.g., "pick a model that supports vision" or "pick the cheapest model that supports tools"). Without a catalog, the agent must hallucinate model identifiers from its training data, which go stale as providers release new models. The catalog gives the agent a reliable, up-to-date source of truth.

The catalog should be shipped as a data file (JSON or similar) that can be updated independently of the library code. Consider auto-generating it from provider documentation or APIs. **When in doubt, prefer the latest models** -- they are generally more capable, and the SDK should make it easy to stay current.

### 2.10 Prompt Caching (Critical for Cost)

Prompt caching allows providers to reuse computation from previous requests when the prefix of the conversation is unchanged. For agentic workloads where the system prompt and conversation history are identical across many turns, caching can reduce input token costs by 50-90%. The unified SDK MUST support caching for each provider.

| Provider  | Caching Behavior                                                      | SDK Action Required |
|-----------|-----------------------------------------------------------------------|---------------------|
| OpenAI    | Automatic -- the Responses API caches shared prefixes server-side     | None. Use the Responses API and report `cache_read_tokens` from usage. |
| Gemini    | Automatic -- prefix caching for repeated content, plus explicit `cachedContent` API for long contexts | None for automatic. Expose explicit caching via `provider_options`. |
| Anthropic | **Not automatic.** Requires explicit `cache_control` annotations on content blocks. | The Anthropic adapter must inject `cache_control` breakpoints automatically for agentic workloads. |

Anthropic is the only provider where the SDK must do extra work. Without cache_control annotations, every turn re-processes the entire system prompt and conversation history at full price. With proper caching, cached input tokens cost 90% less. This is the single highest-ROI optimization for agentic workloads.

All three providers report cache statistics. The SDK must map these to `Usage.cache_read_tokens` and `Usage.cache_write_tokens` so callers can verify caching is working.

---

## 3. Data Model

This section defines all types used by the library. The notation uses a language-neutral struct/record style. Field types use these conventions:

- `String` -- text
- `Integer` -- whole number
- `Float` -- decimal number
- `Boolean` -- true/false
- `Bytes` -- raw binary data
- `Dict` -- key-value map
- `List<T>` -- ordered collection of T
- `T | None` -- optional (nullable) value
- `T | U` -- union / either type

### 3.1 Message

The fundamental unit of conversation. A conversation is an ordered `List<Message>`.

```
RECORD Message:
    role          : Role                  -- who produced this message
    content       : List<ContentPart>     -- the message body (multimodal)
    name          : String | None         -- for tool messages and developer attribution
    tool_call_id  : String | None         -- links a tool-result message to its tool call
```

#### Convenience Constructors

For common cases, factory methods create properly structured Message objects:

```
Message.system("You are a helpful assistant.")
Message.user("What is 2 + 2?")
Message.assistant("The answer is 4.")
Message.tool_result(tool_call_id = "call_123", content = "72F and sunny", is_error = false)
```

#### Text Accessor

A convenience property on Message that concatenates text from all text content parts:

```
message.text -> String
    -- Returns the concatenation of all ContentPart entries where kind == TEXT.
    -- Returns empty string if no text parts exist.
```

### 3.2 Role

Five roles cover the semantics of all major providers:

```
ENUM Role:
    SYSTEM       -- High-level instructions shaping model behavior. Typically first.
    USER         -- Human input. Text, images, audio, documents.
    ASSISTANT    -- Model output. Text, tool calls, thinking blocks.
    TOOL         -- Tool execution results, linked by tool_call_id.
    DEVELOPER    -- Privileged instructions from the application (not the end user).
```

Provider mapping for roles:

| SDK Role    | OpenAI               | Anthropic                          | Gemini                    |
|-------------|----------------------|------------------------------------|---------------------------|
| SYSTEM      | `system` role        | Extracted to `system` parameter    | `systemInstruction`       |
| USER        | `user` role          | `user` role                        | `user` role               |
| ASSISTANT   | `assistant` role     | `assistant` role                   | `model` role              |
| TOOL        | `tool` role          | `tool_result` block in user msg    | `functionResponse` in user|
| DEVELOPER   | `developer` role     | Merged with system                 | Merged with system        |

### 3.3 ContentPart (Tagged Union)

Each message contains a list of ContentPart objects. Using a list rather than a single string enables multimodal messages (text interleaved with images), structured assistant responses (text interleaved with tool calls and thinking blocks), and tool results that include images.

ContentPart uses a tagged-union pattern: the `kind` field determines which data field is populated.

```
RECORD ContentPart:
    kind          : ContentKind | String  -- discriminator tag
    text          : String | None         -- populated when kind == TEXT
    image         : ImageData | None      -- populated when kind == IMAGE
    audio         : AudioData | None      -- populated when kind == AUDIO
    document      : DocumentData | None   -- populated when kind == DOCUMENT
    tool_call     : ToolCallData | None   -- populated when kind == TOOL_CALL
    tool_result   : ToolResultData | None -- populated when kind == TOOL_RESULT
    thinking      : ThinkingData | None   -- populated when kind == THINKING or REDACTED_THINKING
```

Note: The `kind` field accepts both the enum and arbitrary strings. This allows extension for provider-specific content kinds without modifying the core enum.

### 3.4 ContentKind

```
ENUM ContentKind:
    TEXT                -- Plain text. The most common kind.
    IMAGE               -- Image as URL, base64, or file reference.
    AUDIO               -- Audio as URL or raw bytes with media type.
    DOCUMENT            -- Document (PDF, etc.) as URL, base64, or file reference.
    TOOL_CALL           -- A model-initiated tool invocation.
    TOOL_RESULT         -- The result of executing a tool call.
    THINKING            -- Model reasoning/thinking content.
    REDACTED_THINKING   -- Redacted reasoning (Anthropic). Opaque, must round-trip verbatim.
```

Direction constraints:

| Kind              | May appear in roles          |
|-------------------|------------------------------|
| TEXT              | SYSTEM, USER, ASSISTANT, DEVELOPER, TOOL |
| IMAGE             | USER (input), ASSISTANT (generated) |
| AUDIO             | USER (input)                 |
| DOCUMENT          | USER (input)                 |
| TOOL_CALL         | ASSISTANT (output)           |
| TOOL_RESULT       | TOOL (response)              |
| THINKING          | ASSISTANT (output)           |
| REDACTED_THINKING | ASSISTANT (output)           |

### 3.5 Content Data Structures

#### ImageData

```
RECORD ImageData:
    url         : String | None     -- URL pointing to the image
    data        : Bytes | None      -- raw image bytes
    media_type  : String | None     -- MIME type, e.g. "image/png", "image/jpeg"
    detail      : String | None     -- processing fidelity hint: "auto", "low", "high"
```

Exactly one of `url` or `data` must be provided. The adapter base64-encodes `data` if the provider requires it. `media_type` defaults to `"image/png"` when `data` is provided and no type is specified.

**Image upload is critical for multimodal capabilities.** Many models (Claude, GPT-4.1, Gemini) accept image inputs for analysis, code screenshot reading, diagram understanding, and more. The SDK must handle image upload correctly across all providers:

| Concern              | OpenAI                                               | Anthropic                                         | Gemini                                            |
|----------------------|------------------------------------------------------|---------------------------------------------------|---------------------------------------------------|
| URL images           | `image_url.url` field                                | `source.type = "url"` with `url` field            | `fileData.fileUri` field                          |
| Base64 images        | `image_url.url` as data URI (`data:mime;base64,...`) | `source.type = "base64"` with `data` + `media_type` | `inlineData` with `data` + `mimeType`           |
| File path (local)    | Read file, base64-encode, send as data URI           | Read file, base64-encode, send as base64 source   | Read file, base64-encode, send as inlineData     |
| Supported formats    | PNG, JPEG, GIF, WEBP                                | PNG, JPEG, GIF, WEBP                              | PNG, JPEG, GIF, WEBP, HEIC, HEIF                |
| Max image size       | 20MB                                                 | ~5MB per image (base64 encoded)                   | Varies by method                                  |
| Detail/fidelity hint | `detail`: "auto", "low", "high"                     | Not supported (ignore)                            | Not supported (ignore)                            |

**Convenience: file path support.** The SDK should accept a local file path as a convenience. When `url` looks like a local file path (starts with `/`, `./`, or `~`), the adapter reads the file, infers the MIME type from the extension, base64-encodes the contents, and sends it using the provider's inline data format. This makes it easy for coding agents to send screenshots and diagrams without manual encoding.

#### AudioData

```
RECORD AudioData:
    url         : String | None
    data        : Bytes | None
    media_type  : String | None     -- e.g. "audio/wav", "audio/mp3"
```

#### DocumentData

```
RECORD DocumentData:
    url         : String | None
    data        : Bytes | None
    media_type  : String | None     -- e.g. "application/pdf"
    file_name   : String | None     -- optional display name
```

#### ToolCallData

```
RECORD ToolCallData:
    id              : String            -- unique identifier for this call (provider-assigned)
    name            : String            -- tool name
    arguments       : Dict              -- parsed JSON arguments
    raw_arguments   : String | None     -- raw argument string before parsing (for debugging)
```

The `id` field is assigned by the provider and is required for linking tool results back to calls. For providers that do not assign unique IDs (e.g., Gemini), the adapter must generate synthetic unique IDs (e.g., `"call_" + random_uuid()`) and maintain a mapping to the function name.

#### ToolResultData

```
RECORD ToolResultData:
    tool_call_id    : String            -- the ToolCallData.id this result answers
    content         : String | Dict     -- the tool's output (text or structured)
    is_error        : Boolean           -- whether the tool execution failed
    image_data      : Bytes | None      -- optional image result
    image_media_type: String | None     -- MIME type for the image result
```

When `is_error` is true, the model understands the tool failed and can adjust its approach.

#### ThinkingData

```
RECORD ThinkingData:
    text        : String            -- the thinking/reasoning content
    signature   : String | None     -- provider-specific signature for round-tripping
    redacted    : Boolean           -- true if this is redacted thinking (opaque content)
```

Thinking blocks from Anthropic's extended thinking must be preserved exactly as received and included in subsequent messages. The `signature` field enables this. Redacted thinking blocks contain opaque data that cannot be read but must be passed back verbatim.

**Cross-provider portability:** Thinking blocks with signatures are only valid when continuing with the same provider and model. When switching providers, the adapter should strip signatures and optionally convert the thinking text to a user-visible context message.

### 3.6 Request

The single input type for both `complete()` and `stream()`:

```
RECORD Request:
    model             : String                      -- required; provider's native model ID
    messages          : List<Message>               -- required; the conversation
    provider          : String | None               -- optional; uses default if omitted
    tools             : List<ToolDefinition> | None -- optional
    tool_choice       : ToolChoice | None           -- optional; defaults to AUTO if tools present
    response_format   : ResponseFormat | None       -- optional; text, json, or json_schema
    temperature       : Float | None
    top_p             : Float | None
    max_tokens        : Integer | None
    stop_sequences    : List<String> | None
    reasoning_effort  : String | None               -- "none", "low", "medium", "high"
    metadata          : Dict<String, String> | None -- arbitrary key-value pairs
    provider_options  : Dict | None                 -- escape hatch for provider-specific params
```

#### Provider Options (Escape Hatch)

The `provider_options` field passes through provider-specific parameters that the unified interface does not model. Each adapter extracts the options it understands and ignores the rest.

```
request = Request(
    model = "claude-opus-4-6",
    messages = [ ... ],
    provider_options = {
        "anthropic": {
            "thinking": { "type": "enabled", "budget_tokens": 10000 },
            "beta_features": ["interleaved-thinking-2025-05-14"]
        }
    }
)
```

Code that uses `provider_options` is explicitly not portable. The library documents this tradeoff.

### 3.7 Response

```
RECORD Response:
    id              : String                -- provider-assigned response ID
    model           : String                -- actual model used (may differ from requested)
    provider        : String                -- which provider fulfilled the request
    message         : Message               -- the assistant's response as a Message
    finish_reason   : FinishReason          -- why generation stopped
    usage           : Usage                 -- token counts
    raw             : Dict | None           -- raw provider response JSON (for debugging)
    warnings        : List<Warning>         -- non-fatal issues (optional, may be empty)
    rate_limit      : RateLimitInfo | None  -- rate limit metadata from headers (optional)
```

Convenience accessors on Response:

```
response.text        -> String              -- concatenated text from all text parts
response.tool_calls  -> List<ToolCall>      -- extracted tool calls from the message
response.reasoning   -> String | None       -- concatenated reasoning/thinking text
```

### 3.8 FinishReason

A dual representation preserving both portable semantics and provider-specific detail:

```
RECORD FinishReason:
    reason  : String        -- unified: one of the values below
    raw     : String | None -- the provider's native finish reason string
```

**Note for statically-typed languages:** An enum (discriminated union) with variants `Stop`, `Length`, `ToolCalls`, `ContentFilter`, `Error`, and `Other(String)` is an acceptable representation. The provider's raw finish reason string is available in `Response.raw` (the full provider response). A separate `raw` field on FinishReason itself is optional in typed implementations.

Unified reason values:

| Value            | Meaning                                      |
|------------------|----------------------------------------------|
| `stop`           | Natural end of generation (model stopped)    |
| `length`         | Output reached max_tokens limit              |
| `tool_calls`     | Model wants to invoke one or more tools      |
| `content_filter` | Response blocked by safety/content filter     |
| `error`          | An error occurred during generation          |
| `other`          | Provider-specific reason not mapped above    |

Provider finish reason mapping:

| Provider  | Provider Value    | Unified Value    |
|-----------|-------------------|------------------|
| OpenAI (Responses API) | completed   | stop             |
| OpenAI (Responses API) | incomplete  | length           |
| OpenAI (Responses API) | failed      | error            |
| OpenAI (Responses API) | (has function_call items) | tool_calls |
| OpenAI (Chat Completions) | stop     | stop             |
| OpenAI (Chat Completions) | length   | length           |
| OpenAI (Chat Completions) | tool_calls | tool_calls     |
| OpenAI (Chat Completions) | content_filter | content_filter |
| Anthropic | end_turn          | stop             |
| Anthropic | stop_sequence     | stop             |
| Anthropic | max_tokens        | length           |
| Anthropic | tool_use          | tool_calls       |
| Gemini    | STOP              | stop             |
| Gemini    | MAX_TOKENS        | length           |
| Gemini    | SAFETY            | content_filter   |
| Gemini    | RECITATION        | content_filter   |
| Gemini    | (has tool calls)  | tool_calls       |

Note: Gemini does not have a dedicated "tool_calls" finish reason. The adapter infers it from the presence of `functionCall` parts in the response. Similarly, the OpenAI Responses API uses `completed`/`incomplete`/`failed` status rather than Chat Completions-style finish reasons, and tool calls are inferred from the presence of `function_call` output items.

### 3.9 Usage

```
RECORD Usage:
    input_tokens        : Integer           -- tokens in the prompt
    output_tokens       : Integer           -- tokens generated by the model
    total_tokens        : Integer           -- input + output
    reasoning_tokens    : Integer | None    -- tokens used for chain-of-thought reasoning
    cache_read_tokens   : Integer | None    -- tokens served from prompt cache
    cache_write_tokens  : Integer | None    -- tokens written to prompt cache
    raw                 : Dict | None       -- raw provider usage data
```

Usage objects must support addition for aggregating across multi-step operations:

```
usage_a + usage_b -> Usage
    -- Sums integer fields.
    -- For optional fields: if either side is non-None, sum them (treating None as 0).
    -- If both sides are None for an optional field, the result is None.
```

Provider usage field mapping:

| SDK Field           | OpenAI Field                                         | Anthropic Field                  | Gemini Field                          |
|---------------------|------------------------------------------------------|----------------------------------|---------------------------------------|
| input_tokens        | usage.input_tokens                                   | usage.input_tokens               | usageMetadata.promptTokenCount        |
| output_tokens       | usage.output_tokens                                  | usage.output_tokens              | usageMetadata.candidatesTokenCount    |
| reasoning_tokens    | usage.completion_tokens_details.reasoning_tokens     | (see note below)                 | usageMetadata.thoughtsTokenCount      |
| cache_read_tokens   | usage.prompt_tokens_details.cached_tokens            | usage.cache_read_input_tokens    | usageMetadata.cachedContentTokenCount |
| cache_write_tokens  | (not provided)                                       | usage.cache_creation_input_tokens| (not provided)                        |

#### Reasoning Token Handling (Critical)

Reasoning tokens are tokens the model uses for internal chain-of-thought before producing visible output. Properly tracking and surfacing reasoning tokens is essential for cost management and debugging, because reasoning tokens are billed as output tokens but are not visible in the response text.

**OpenAI reasoning models (GPT-5.2 series, etc.):**
- The **Responses API** (`/v1/responses`) is REQUIRED for reasoning models. The Chat Completions API does not return reasoning token breakdowns for these models. The Responses API returns `usage.output_tokens_details.reasoning_tokens` which tells you exactly how many tokens were spent on reasoning vs. visible output.
- The `reasoning_effort` request parameter ("low", "medium", "high") controls how much reasoning the model does. This maps to `reasoning.effort` in the Responses API request body.
- Reasoning content is not visible in the response (OpenAI does not expose the thinking text for GPT-5.2 series models). The adapter should still populate `reasoning_tokens` in Usage so callers can track costs.

**Anthropic extended thinking (Claude with thinking enabled):**
- Extended thinking is enabled via the `thinking` parameter (through `provider_options`) and requires specific beta headers.
- Anthropic surfaces thinking as explicit `thinking` content blocks in the response. These blocks contain the actual reasoning text and count toward `output_tokens` in the usage.
- The adapter should populate `reasoning_tokens` by summing the token lengths of thinking blocks (Anthropic does not provide a separate reasoning token count, but the thinking block text can be used for estimation).
- Thinking blocks carry a `signature` field that must be round-tripped verbatim in subsequent messages.

**Gemini thinking (Gemini 3 models):**
- Gemini 3 Flash supports "thinking" via the `thinkingConfig` parameter.
- Gemini reports `thoughtsTokenCount` in `usageMetadata`, which maps directly to `reasoning_tokens`.
- Thinking content may be returned in the response as a `thought` part.

**Why this matters:** When switching between providers, reasoning token usage can vary dramatically. A query that uses 500 reasoning tokens on OpenAI GPT-5.2 might use 2000 thinking tokens on Claude. The unified SDK must track this accurately so callers can make informed cost decisions. Even though reasoning tokens make direct provider switching unfavorable (the thinking styles are different), the SDK should still translate correctly so higher-level tools can compare.

### 3.10 ResponseFormat

```
RECORD ResponseFormat:
    type        : String            -- "text", "json", or "json_schema"
    json_schema : Dict | None       -- required when type is "json_schema"
    strict      : Boolean           -- when true, provider enforces schema strictly (default: false)
```

### 3.11 Warning

```
RECORD Warning:
    message : String                -- human-readable description of the non-fatal issue
    code    : String | None         -- machine-readable warning code
```

### 3.12 RateLimitInfo

```
RECORD RateLimitInfo:
    requests_remaining  : Integer | None
    requests_limit      : Integer | None
    tokens_remaining    : Integer | None
    tokens_limit        : Integer | None
    reset_at            : Timestamp | None
```

Populated from provider response headers (e.g., `x-ratelimit-remaining-requests`). This data is informational; the library does not use it for proactive throttling.

Note: OpenAI field names shown above are for the Responses API (`/v1/responses`). The Chat Completions API uses `usage.prompt_tokens` and `usage.completion_tokens` instead.

### 3.13 StreamEvent

All stream events share a `type` discriminator field. The library normalizes provider-specific SSE formats into this unified event model.

```
RECORD StreamEvent:
    type              : StreamEventType | String

    -- text events
    delta             : String | None           -- incremental text
    text_id           : String | None           -- identifies which text segment this belongs to

    -- reasoning events
    reasoning_delta   : String | None           -- incremental reasoning/thinking text

    -- tool call events
    tool_call         : ToolCall | None         -- partial or complete tool call

    -- finish event
    finish_reason     : FinishReason | None
    usage             : Usage | None
    response          : Response | None         -- the full accumulated response

    -- error event
    error             : SDKError | None

    -- passthrough
    raw               : Dict | None             -- raw provider event for passthrough
```

Note: Statically-typed implementations may use flat discriminated-union fields (`toolCallId`, `toolName`, `argumentsDelta`) instead of a shared nullable `tool_call` field, as flat fields enable better type narrowing.

### 3.14 StreamEventType

```
ENUM StreamEventType:
    STREAM_START        -- Stream has begun. May include warnings.
    TEXT_START           -- A new text segment has begun. Includes text_id.
    TEXT_DELTA           -- Incremental text content. Includes delta and text_id.
    TEXT_END             -- Text segment is complete. Includes text_id.
    REASONING_START     -- Model reasoning has begun.
    REASONING_DELTA     -- Incremental reasoning content.
    REASONING_END       -- Reasoning is complete.
    TOOL_CALL_START     -- A tool call has begun. Includes tool name and call ID.
    TOOL_CALL_DELTA     -- Incremental tool call arguments (partial JSON).
    TOOL_CALL_END       -- Tool call is fully formed and ready for execution.
    STEP_FINISH         -- A tool execution step completed. Includes tool calls and results from this step.
    FINISH              -- Generation complete. Includes finish_reason, usage, response.
    ERROR               -- An error occurred during streaming.
    PROVIDER_EVENT      -- Raw provider event not mapped to the unified model.
```

**The start/delta/end pattern.** Text, reasoning, and tool call events follow a consistent start/delta/end lifecycle. This pattern enables:

1. **Multiple concurrent segments** -- a response can contain multiple text segments or tool calls in flight simultaneously. IDs correlate deltas to their segment.
2. **Resource lifecycle** -- consumers know when a segment begins and ends, enabling proper buffer management and UI updates.
3. **Typed completion** -- the end event carries the final accumulated value for its segment.

Consumers that only care about text deltas can filter for `TEXT_DELTA` events and ignore start/end events.

---

## 4. Generation and Streaming

### 4.1 Low-Level: Client.complete()

The fundamental blocking call. Sends a request, blocks until the model finishes, returns the full response.

```
response = client.complete(Request(
    model = "claude-opus-4-6",
    messages = [Message.user("Explain photosynthesis in one paragraph")],
    max_tokens = 500,
    temperature = 0.7
))

response.text           -- "Photosynthesis is..."
response.finish_reason  -- FinishReason(reason="stop", raw="end_turn")
response.usage          -- Usage(input_tokens=12, output_tokens=85, ...)
```

**Behavior:**
- Routes to the resolved provider adapter.
- Blocks until the model produces a complete response.
- Returns a Response object.
- Raises an exception on provider errors.
- Does NOT retry automatically. Retries are the responsibility of Layer 4 (high-level API) or application code.

### 4.2 Low-Level: Client.stream()

The fundamental streaming call. Returns an asynchronous iterator of StreamEvent objects.

```
event_stream = client.stream(Request(
    model = "claude-opus-4-6",
    messages = [Message.user("Write a short story")]
))

FOR EACH event IN event_stream:
    IF event.type == TEXT_DELTA:
        PRINT(event.delta)
    ELSE IF event.type == FINISH:
        PRINT("Done. Tokens: " + event.usage.total_tokens)
```

**Behavior:**
- Returns an async iterator immediately.
- Yields StreamEvent objects as they arrive from the provider.
- The stream terminates with a FINISH event containing the complete accumulated response.
- Must be consumed or explicitly closed; abandoning a stream without closing it may leak connections.
- Does NOT retry automatically.

### 4.3 High-Level: generate()

The primary blocking generation function. Wraps `Client.complete()` with tool execution loops, multi-step orchestration, prompt standardization, and automatic retries.

```
FUNCTION generate(
    model             : String,
    prompt            : String | None,               -- simple text prompt
    messages          : List<Message> | None,        -- full message history
    system            : String | None,               -- system message
    tools             : List<Tool> | None,           -- tools with optional execute handlers
    tool_choice       : ToolChoice | None,           -- auto/none/required/named
    max_tool_rounds   : Integer = 1,                 -- max tool execution loop iterations
    stop_when         : StopCondition | None,        -- custom stop condition for tool loops
    response_format   : ResponseFormat | None,
    temperature       : Float | None,
    top_p             : Float | None,
    max_tokens        : Integer | None,
    stop_sequences    : List<String> | None,
    reasoning_effort  : String | None,
    provider          : String | None,
    provider_options  : Dict | None,
    max_retries       : Integer = 2,                 -- retry count for transient errors
    timeout           : Float | TimeoutConfig | None,
    abort_signal      : AbortSignal | None,          -- cancellation signal
    client            : Client | None                -- override default client
) -> GenerateResult
```

**Prompt standardization:** Either `prompt` (a simple string, converted to a single user message) or `messages` (full conversation) is provided, not both. Using both is an error. The `system` parameter is always separate and prepended as a system message.

**Tool execution loop (detailed in Section 5):** When tools with execute handlers are provided and the model responds with tool calls, `generate()` automatically executes the tools, appends their results to the conversation, and calls the model again. This loop continues until the model responds without tool calls, `max_tool_rounds` is reached, or a stop condition is met.

**`max_tool_rounds` semantics:** The value represents the maximum number of times tool calls are executed and results are fed back. A value of 1 means: make the initial call, if the model returns tool calls execute them and make one more call. A value of 0 means no automatic tool execution (tools are returned to the caller). The total number of LLM calls is at most `max_tool_rounds + 1`.

#### GenerateResult

```
RECORD GenerateResult:
    text            : String                    -- text from the final step
    reasoning       : String | None             -- reasoning from the final step
    tool_calls      : List<ToolCall>            -- tool calls from the final step
    tool_results    : List<ToolResult>          -- tool results from the final step
    finish_reason   : FinishReason
    usage           : Usage                     -- usage from the final step
    total_usage     : Usage                     -- aggregated usage across ALL steps
    steps           : List<StepResult>          -- detailed results for each step
    response        : Response                  -- the final Response object
    output          : Any | None                -- parsed structured output (for generate_object)
```

#### StepResult

```
RECORD StepResult:
    text            : String
    reasoning       : String | None
    tool_calls      : List<ToolCall>
    tool_results    : List<ToolResult>
    finish_reason   : FinishReason
    usage           : Usage
    response        : Response
    warnings        : List<Warning>
```

### 4.4 High-Level: stream()

The primary streaming generation function. Equivalent to `generate()` but yields events incrementally.

```
result = stream(
    model = "claude-opus-4-6",
    prompt = "Write a haiku about coding"
)

FOR EACH event IN result:
    IF event.type == TEXT_DELTA:
        PRINT(event.delta)

-- After iteration, the full response is available:
response = result.response()
```

Accepts the same parameters as `generate()`. When tools with execute handlers are provided and the model makes tool calls, the stream pauses while tools execute, emits a `step_finish` event, then resumes streaming the model's next response.

The returned StreamResult provides:
- Async iteration over events.
- `response()` -- returns the accumulated Response after the stream ends.
- `text_stream` -- an async iterable that yields only text deltas (convenience).

#### StreamResult

```
RECORD StreamResult:
    ASYNC ITERATOR over StreamEvent
    FUNCTION response() -> Response         -- accumulated response (available after stream ends)
    PROPERTY text_stream -> AsyncIterator<String>  -- yields only text deltas
    PROPERTY partial_response -> Response | None   -- current accumulated state at any point
```

#### StreamAccumulator

A utility that collects stream events into a complete Response:

```
accumulator = StreamAccumulator()

FOR EACH event IN stream:
    accumulator.process(event)

response = accumulator.response()   -- equivalent to what complete() would return
```

This bridges the two modes: any code that works with a Response can be used with streaming by accumulating first.

### 4.5 High-Level: generate_object()

Structured output generation with schema validation:

```
result = generate_object(
    model = "gpt-5.2",
    prompt = "Extract the person's name and age from: 'Alice is 30 years old'",
    schema = {
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" }
        },
        "required": ["name", "age"]
    }
)

result.output   -- { "name": "Alice", "age": 30 }  (parsed and validated)
result.text     -- raw text response
```

**Implementation strategy by provider:**

| Provider  | Strategy                                                                    |
|-----------|-----------------------------------------------------------------------------|
| OpenAI    | Native `response_format: { type: "json_schema", ... }` with strict mode    |
| Gemini    | Native `responseMimeType: "application/json"` with `responseSchema`        |
| Anthropic | Fallback: inject schema instructions into the system prompt, parse output. Alternatively, use tool-based extraction (define a tool whose input schema matches the desired output, force the model to call it). |

If parsing or validation fails, the function raises `NoObjectGeneratedError`.

### 4.6 High-Level: stream_object()

Streaming structured output with partial object updates:

```
result = stream_object(
    model = "gpt-5.2",
    prompt = "Generate a list of 5 recipes",
    schema = recipes_schema
)

FOR EACH partial IN result:
    -- partial is a partially-parsed object that grows as tokens arrive
    PRINT("Recipes so far: " + LENGTH(partial.recipes))

final = result.object()  -- the complete, validated object
```

Uses incremental JSON parsing to yield partial objects as tokens arrive. This enables progressive UI rendering.

### 4.7 Cancellation and Timeouts

#### Abort Signals

Both `generate()` and `stream()` accept an abort signal for cooperative cancellation:

```
controller = AbortController()

-- In another thread/coroutine:
controller.abort()

-- The generate call raises AbortError if cancelled:
result = generate(model = "...", prompt = "...", abort_signal = controller.signal)
```

For streaming, cancellation closes the underlying connection and the stream raises AbortError.

#### Timeouts

Timeouts can be specified as a simple duration (total timeout) or a structured config:

```
RECORD TimeoutConfig:
    total       : Float | None      -- max time for the entire multi-step operation
    per_step    : Float | None      -- max time per individual LLM call
```

The library distinguishes three timeout scopes at the adapter level:

```
RECORD AdapterTimeout:
    connect     : Float             -- time to establish HTTP connection (default: 10s)
    request     : Float             -- time for entire request/response cycle (default: 120s)
    stream_read : Float             -- max time between consecutive stream events (default: 30s)
```

---

## 5. Tool Calling

### 5.1 Tool Definition

```
RECORD Tool:
    name        : String                    -- unique identifier; [a-zA-Z][a-zA-Z0-9_]* max 64 chars
    description : String                    -- human-readable description for the model
    parameters  : Dict                      -- JSON Schema defining the input (root must be "object")
    execute     : Function | None           -- handler function (if present, tool is "active")
```

**Tool name constraints:** Names must be valid identifiers: alphanumeric characters and underscores, starting with a letter. Maximum 64 characters. This is the strictest common subset across all providers. The library validates names at definition time.

**Parameter schema:** Parameters must be defined as a JSON Schema object with `"type": "object"` at the root. This is a universal requirement across all providers. The library passes this schema to the provider, which uses it to constrain argument generation.

**Example:**

```
weather_tool = Tool(
    name = "get_weather",
    description = "Get the current weather for a location",
    parameters = {
        "type": "object",
        "properties": {
            "location": {
                "type": "string",
                "description": "City name, e.g. 'San Francisco, CA'"
            },
            "unit": {
                "type": "string",
                "enum": ["celsius", "fahrenheit"],
                "description": "Temperature unit"
            }
        },
        "required": ["location"]
    },
    execute = get_weather_function
)
```

### 5.2 Tool Execute Handlers

The `execute` handler is a callable (sync or async) that receives parsed arguments and returns a result:

```
FUNCTION get_weather(location: String, unit: String = "celsius") -> String:
    -- Call weather API...
    RETURN "72F and sunny in " + location
```

**Handler contract:**
- **Input:** Parsed JSON arguments as keyword arguments, or a single dictionary.
- **Output:** A string, dictionary, list, or any JSON-serializable value.
- **Errors:** Raise an exception to indicate tool failure. The library catches it and sends an error result to the model (with `is_error = true`), allowing the model to recover.

**Tool context injection:** Handlers can optionally receive injected context. The library inspects the handler's signature and injects recognized keyword arguments:

```
FUNCTION my_tool(
    query        : String,          -- tool parameter
    messages     : List<Message>,   -- injected: current conversation
    abort_signal : AbortSignal,     -- injected: cancellation signal
    tool_call_id : String           -- injected: ID of this call
) -> String:
    ...
```

### 5.3 ToolChoice

Controls whether and how the model uses tools:

```
RECORD ToolChoice:
    mode        : String            -- "auto", "none", "required", "named"
    tool_name   : String | None     -- required when mode is "named"
```

| Mode     | Behavior                                                      |
|----------|---------------------------------------------------------------|
| auto     | Model decides whether to call tools or respond with text.     |
| none     | Model must not call any tools, even if defined.               |
| required | Model must call at least one tool.                            |
| named    | Model must call the specific tool identified by tool_name.    |

Provider mapping:

| SDK Mode  | OpenAI                                                  | Anthropic                          | Gemini                                                     |
|-----------|---------------------------------------------------------|------------------------------------|------------------------------------------------------------|
| auto      | `"auto"`                                                | `{"type": "auto"}`                 | `"AUTO"`                                                   |
| none      | `"none"`                                                | Omit tools from request            | `"NONE"`                                                   |
| required  | `"required"`                                            | `{"type": "any"}`                  | `"ANY"`                                                    |
| named     | `{"type":"function","name":"..."}`                     | `{"type":"tool","name":"..."}`     | `{"mode":"ANY","allowedFunctionNames":["..."]}`            |

Note on Anthropic `none` mode: Anthropic does not support `tool_choice: {"type": "none"}` when tools are present. The adapter must omit the tools array from the request body entirely.

If a provider does not support a particular mode, the adapter raises `UnsupportedToolChoiceError`. The `supports_tool_choice(mode)` method allows checking capabilities upfront.

### 5.4 ToolCall and ToolResult

Extracted from responses and produced by execute handlers:

```
RECORD ToolCall:
    id              : String            -- unique identifier (provider-assigned)
    name            : String            -- tool name
    arguments       : Dict              -- parsed JSON arguments
    raw_arguments   : String | None     -- raw argument string before parsing
```

```
RECORD ToolResult:
    tool_call_id    : String            -- correlates to ToolCall.id
    content         : String | Dict | List  -- the tool's output
    is_error        : Boolean           -- true if the tool execution failed
```

### 5.5 Active Tools vs Passive Tools

**Active tools** have an `execute` handler. When used with `generate()` or `stream()`, the library automatically executes them and loops until the model produces a final text response.

**Passive tools** have no `execute` handler. Tool calls are returned to the caller in the response, and the caller manages the execution loop manually using `Client.complete()`.

Passive tools are useful when:
- Tool execution requires external coordination (human approval, external orchestration).
- The calling code has its own loop and state management.
- Tools need to be executed in a specific order or with side effects between them.

### 5.6 Multi-Step Tool Loop

When `generate()` is called with active tools, the following loop executes:

```
FUNCTION tool_loop(request, tools, max_tool_rounds, stop_when):
    conversation = request.messages
    steps = []

    FOR round_num FROM 0 TO max_tool_rounds:
        response = client.complete(request_with(conversation))
        tool_calls = response.tool_calls

        -- Execute tools if the model wants to call them
        IF tool_calls AND response.finish_reason.reason == "tool_calls":
            tool_results = execute_all_tools(tools, tool_calls)  -- concurrent
        ELSE:
            tool_results = []

        step = StepResult(response, tool_calls, tool_results, ...)
        steps.APPEND(step)

        -- Check stop conditions
        IF tool_calls is empty OR response.finish_reason.reason != "tool_calls":
            BREAK   -- model is done (natural completion)
        IF round_num >= max_tool_rounds:
            BREAK   -- budget exhausted
        IF stop_when is not None AND stop_when(steps) == true:
            BREAK   -- custom stop condition met

        -- Continue conversation with tool results
        conversation.APPEND(response.message)            -- assistant message with tool calls
        FOR EACH result IN tool_results:
            conversation.APPEND(Message.tool_result(
                tool_call_id = result.tool_call_id,
                content = result.content,
                is_error = result.is_error
            ))

    RETURN GenerateResult from steps
```

### 5.7 Parallel Tool Execution

When the model returns multiple tool calls in a single response, they are logically independent (the model generated them simultaneously without seeing any results). The library MUST handle this correctly:

1. **Execute all tool calls concurrently.** Launch all execute handlers simultaneously (using async tasks, threads, or equivalent concurrency primitive).
2. **Wait for ALL results before continuing.** Do not send partial results back to the model. The continuation request must include results for every tool call from the previous response.
3. **Send all results in a single continuation request.** Bundle all tool results into the message history and make one LLM call, not one call per result.
4. **Preserve ordering.** Tool results should appear in the same order as the corresponding tool calls, even though execution may complete out of order.
5. **Handle partial failures gracefully.** If some tool executions succeed and others fail, send all results (with `is_error = true` for failures). Do not abort the entire batch because one tool failed.

```
FUNCTION execute_all_tools(tools, tool_calls):
    -- Launch all executions concurrently
    futures = []
    FOR EACH call IN tool_calls:
        tool = find_tool(tools, call.name)
        IF tool AND tool.execute:
            futures.APPEND(async_execute(tool.execute, call.arguments, call.id))
        ELSE:
            futures.APPEND(immediate_error(call.id, "Unknown tool: " + call.name))

    -- Wait for ALL to complete
    results = AWAIT_ALL(futures)

    RETURN results   -- List<ToolResult>, one per tool_call, in order
```

This is critical for downstream consumers like coding agents. When a model asks to read three files simultaneously, the SDK handles the concurrent execution and result batching so the coding agent's agentic loop does not have to manage it.

### 5.8 Tool Call Validation and Repair

Before passing arguments to the execute handler, the library:

1. Parses the JSON argument string.
2. Optionally validates against the tool's parameter schema.
3. If validation fails and a `repair_tool_call` function is provided, attempts repair (e.g., ask the model to fix the arguments).
4. If repair fails or is not configured, sends an error result to the model.

**Unknown tool calls:** When the model calls a tool not in the definitions, the library sends an error result rather than raising an exception. This gives the model a chance to correct its behavior.

### 5.9 Streaming with Tools

When streaming with active tools, the stream emits tool call events as they form. Between steps (after tool execution, before the next model call), a `step_finish` event is emitted. The consumer sees a continuous stream of events spanning multiple steps.

### 5.10 Tool Result Handling Across Providers

How tool results are translated to each provider's format:

| SDK Format                           | OpenAI                                | Anthropic                             | Gemini                              |
|--------------------------------------|---------------------------------------|---------------------------------------|-------------------------------------|
| TOOL role message with ToolResultData | Separate `tool` messages with `tool_call_id` | `tool_result` content blocks in `user` message | `functionResponse` parts in `user` content |

---

## 6. Error Handling and Retry

### 6.1 Error Taxonomy

All library errors inherit from a single base:

```
RECORD SDKError:
    message : String                -- human-readable description
    cause   : Exception | None      -- underlying exception, if any
```

Error hierarchy:

```
SDKError
 +-- ProviderError                      -- errors from the LLM provider
 |    +-- AuthenticationError           -- 401: invalid API key, expired token
 |    +-- AccessDeniedError             -- 403: insufficient permissions
 |    +-- NotFoundError                 -- 404: model not found, endpoint not found
 |    +-- InvalidRequestError           -- 400: malformed request, invalid parameters
 |    +-- RateLimitError                -- 429: rate limit exceeded
 |    +-- ServerError                   -- 500-599: provider internal error
 |    +-- ContentFilterError            -- response blocked by safety filter
 |    +-- ContextLengthError            -- input + output exceeds context window
 |    +-- QuotaExceededError            -- billing/usage quota exhausted
 +-- RequestTimeoutError                -- request or stream timed out
 +-- AbortError                         -- request cancelled via abort signal
 +-- NetworkError                       -- network-level failure
 +-- StreamError                        -- error during stream consumption
 +-- InvalidToolCallError               -- tool call arguments failed validation
 +-- NoObjectGeneratedError             -- structured output parsing/validation failed
 +-- ConfigurationError                 -- SDK misconfiguration (missing provider, etc.)
```

Note: Error class names are chosen to avoid shadowing common language built-in names (e.g., `AccessDeniedError` instead of `PermissionError`, `NetworkError` instead of `ConnectionError`, `RequestTimeoutError` instead of `TimeoutError`).

### 6.2 ProviderError Fields

```
RECORD ProviderError extends SDKError:
    provider    : String                -- which provider returned the error
    status_code : Integer | None        -- HTTP status code, if applicable
    error_code  : String | None         -- provider-specific error code
    retryable   : Boolean               -- whether this error is safe to retry
    retry_after : Float | None          -- seconds to wait before retrying
    raw         : Dict | None           -- raw error response body from the provider
```

### 6.3 Retryability Classification

Every error carries a `retryable` property.

**Non-retryable errors** (client mistakes -- retrying will not help):

| Error                  | Status Code | Retryable |
|------------------------|-------------|-----------|
| AuthenticationError    | 401         | false     |
| AccessDeniedError      | 403         | false     |
| NotFoundError          | 404         | false     |
| InvalidRequestError    | 400, 422    | false     |
| ContextLengthError     | 413         | false     |
| QuotaExceededError     | (varies)    | false     |
| ContentFilterError     | (varies)    | false     |
| ConfigurationError     | (N/A)       | false     |

**Retryable errors** (transient -- may succeed on retry):

| Error                  | Status Code | Retryable |
|------------------------|-------------|-----------|
| RateLimitError         | 429         | true      |
| ServerError            | 500-504     | true      |
| RequestTimeoutError    | 408         | true      |
| NetworkError           | (N/A)       | true      |
| StreamError            | (N/A)       | true      |

**Unknown errors default to retryable.** This is a deliberate conservative choice: transient network issues and novel provider error codes are more common than permanent failures from unexpected codes. A false retry is cheaper than a false abort.

### 6.4 HTTP Status Code Mapping

Adapters map HTTP status codes to error types using this table:

| Status | Error Type           | Retryable |
|--------|---------------------|-----------|
| 400    | InvalidRequestError | false     |
| 401    | AuthenticationError | false     |
| 403    | AccessDeniedError   | false     |
| 404    | NotFoundError       | false     |
| 408    | RequestTimeoutError | true      |
| 413    | ContextLengthError  | false     |
| 422    | InvalidRequestError | false     |
| 429    | RateLimitError      | true      |
| 500    | ServerError         | true      |
| 502    | ServerError         | true      |
| 503    | ServerError         | true      |
| 504    | ServerError         | true      |

For Gemini (which may use gRPC status codes):

| gRPC Code           | Error Type           |
|---------------------|---------------------|
| NOT_FOUND           | NotFoundError       |
| INVALID_ARGUMENT    | InvalidRequestError |
| UNAUTHENTICATED     | AuthenticationError |
| PERMISSION_DENIED   | AccessDeniedError   |
| RESOURCE_EXHAUSTED  | RateLimitError      |
| UNAVAILABLE         | ServerError         |
| DEADLINE_EXCEEDED   | RequestTimeoutError |
| INTERNAL            | ServerError         |

### 6.5 Error Message Classification

For ambiguous cases where the status code alone is insufficient, the adapter checks the error message body for classification signals:

- Messages containing "not found" or "does not exist" -> NotFoundError
- Messages containing "unauthorized" or "invalid key" -> AuthenticationError
- Messages containing "context length" or "too many tokens" -> ContextLengthError
- Messages containing "content filter" or "safety" -> ContentFilterError

### 6.6 Retry Policy

```
RECORD RetryPolicy:
    max_retries         : Integer = 2       -- total retry attempts (not counting initial)
    base_delay          : Float = 1.0       -- initial delay in seconds
    max_delay           : Float = 60.0      -- maximum delay between retries
    backoff_multiplier  : Float = 2.0       -- exponential backoff factor
    jitter              : Boolean = true    -- add random jitter to prevent thundering herd
    on_retry            : Callback | None   -- called before each retry with (error, attempt, delay)
```

#### Exponential Backoff with Jitter

The delay for attempt `n` (0-indexed) is calculated as:

```
delay = MIN(base_delay * (backoff_multiplier ^ n), max_delay)
IF jitter:
    delay = delay * RANDOM(0.5, 1.5)   -- +/- 50% jitter
```

Example delays with defaults (base=1.0, multiplier=2.0, max=60.0):

| Attempt | Base Delay | With Jitter (approx range) |
|---------|------------|---------------------------|
| 0       | 1.0s       | 0.5s -- 1.5s              |
| 1       | 2.0s       | 1.0s -- 3.0s              |
| 2       | 4.0s       | 2.0s -- 6.0s              |
| 3       | 8.0s       | 4.0s -- 12.0s             |
| 4       | 16.0s      | 8.0s -- 24.0s             |

#### Retry-After Header

When the provider returns a `Retry-After` header (common with 429 responses):

- If `Retry-After` is less than `max_delay`, use the provider's delay instead of the calculated backoff.
- If `Retry-After` exceeds `max_delay`, do NOT retry. Raise the error immediately with `retry_after` set on the exception. This prevents silently waiting minutes for a rate limit to clear.

#### What Gets Retried

Retries apply to individual LLM calls, not to entire multi-step operations:

- `generate()` with tools: Each step's LLM call is retried independently. A retry on step 3 does not re-execute steps 1 and 2.
- `stream()`: Only the initial connection is retried. Once streaming has begun and partial data has been delivered, the library does not retry. Instead, the stream emits an error event.
- `generate_object()`: The LLM call is retried. Schema validation failures are NOT retried (they indicate a model behavior issue, not a transient error).

#### Retry at the Adapter Level

Provider adapters do NOT retry by default. Retry logic lives in Layer 2 (provider utilities) and is applied by the high-level functions in Layer 4. Low-level `Client.complete()` and `Client.stream()` never retry automatically. Applications using the low-level API can compose retry behavior using a standalone `retry()` utility:

```
response = retry(
    FUNCTION: client.complete(request),
    policy = RetryPolicy(max_retries = 3)
)
```

#### Disabling Retries

Set `max_retries = 0` to disable automatic retries in high-level functions.

### 6.7 Rate Limit Handling

When a provider returns HTTP 429, the library raises RateLimitError with `retry_after` extracted from the response header and `retryable = true`. With automatic retries enabled, rate limits are handled transparently up to the retry budget.

For applications that need proactive rate limiting (staying under limits rather than hitting them), use middleware:

```
FUNCTION rate_limit_middleware(request, next):
    token_bucket.acquire()   -- block until budget available
    RETURN next(request)
```

---

## 7. Provider Adapter Contract

This section provides detailed guidance for implementing a provider adapter. It is intended as a reference for anyone adding support for a new provider.

### 7.1 Interface Summary

Each adapter must implement:

```
INTERFACE ProviderAdapter:
    PROPERTY name : String

    FUNCTION complete(request: Request) -> Response
    FUNCTION stream(request: Request) -> AsyncIterator<StreamEvent>
```

Recommended optional methods:

```
    FUNCTION close() -> Void
    FUNCTION initialize() -> Void
    FUNCTION supports_tool_choice(mode: String) -> Boolean
```

### 7.2 Request Translation

The adapter must translate a unified `Request` into the provider's native API format. The general steps are:

1. **Extract system messages.** For Anthropic: extract from message list, pass as `system` parameter. For Gemini: extract and pass as `systemInstruction`. For OpenAI (Responses API): extract and pass as `instructions` parameter.

2. **Translate messages.** Convert each Message and its ContentParts to the provider's format.

3. **Translate tools.** Convert Tool definitions to the provider's tool format.

4. **Translate tool choice.** Map the unified ToolChoice to the provider's format.

5. **Set generation parameters.** Map temperature, top_p, max_tokens, stop_sequences, etc.

6. **Apply response format.** Translate ResponseFormat to the provider's structured output mechanism.

7. **Apply provider options.** Merge any provider-specific options from `request.provider_options[provider_name]` into the request body.

### 7.3Message Translation Details

#### OpenAI Message Translation (Responses API)

The Responses API uses a different message format than Chat Completions. Messages are passed in an `input` array rather than a `messages` array:

```
Unified Role    -> Responses API Handling
SYSTEM          -> Extracted to `instructions` parameter
USER            -> input item: { "type": "message", "role": "user", "content": [...] }
ASSISTANT       -> input item: { "type": "message", "role": "assistant", "content": [...] }
TOOL            -> input item: { "type": "function_call_output", "call_id": "...", "output": "..." }
DEVELOPER       -> Extracted to `instructions` parameter (or `developer` role input item)

ContentPart Translations:
  TEXT          -> { "type": "input_text", "text": "..." } (user) or { "type": "output_text", "text": "..." } (assistant)
  IMAGE (url)  -> { "type": "input_image", "image_url": "..." }
  IMAGE (data) -> { "type": "input_image", "image_url": "data:<mime>;base64,<data>" }
  TOOL_CALL    -> input item: { "type": "function_call", "id": "...", "name": "...", "arguments": "..." }
  TOOL_RESULT  -> input item: { "type": "function_call_output", "call_id": "...", "output": "..." }
```

Special behaviors:
- System messages are extracted to the `instructions` parameter, not included in the `input` array.
- The `reasoning.effort` parameter controls reasoning for o-series models ("low", "medium", "high").
- Tool calls and results are top-level input items, not nested within messages.
- For third-party OpenAI-compatible endpoints, use the Chat Completions format instead (see Section 7.10).

#### Anthropic Message Translation

```
Unified Role    -> Anthropic Handling
SYSTEM          -> Extracted to `system` parameter (not in messages array)
DEVELOPER       -> Merged with system parameter
USER            -> "user" role
ASSISTANT       -> "assistant" role
TOOL            -> "user" role with tool_result content blocks

ContentPart Translations:
  TEXT          -> { "type": "text", "text": "..." }
  IMAGE (url)  -> { "type": "image", "source": { "type": "url", "url": "..." } }
  IMAGE (data) -> { "type": "image", "source": { "type": "base64", "media_type": "...", "data": "..." } }
  TOOL_CALL    -> { "type": "tool_use", "id": "...", "name": "...", "input": { ... } }
  TOOL_RESULT  -> { "type": "tool_result", "tool_use_id": "...", "content": "...", "is_error": ... }
  THINKING     -> { "type": "thinking", "thinking": "...", "signature": "..." }
  REDACTED_THINKING -> { "type": "redacted_thinking", "data": "..." }
```

Special behaviors:
- **Strict alternation:** Anthropic requires alternating user/assistant messages. The adapter must merge consecutive same-role messages by combining their content arrays.
- **Tool results in user messages:** Anthropic requires tool results to appear in user-role messages, not a separate "tool" role.
- **Thinking block round-tripping:** Thinking and redacted_thinking blocks from previous responses must be preserved exactly as received and included in subsequent assistant messages.
- **max_tokens is required:** Anthropic always requires `max_tokens`. Default to 4096 if not specified.

#### Gemini Message Translation

```
Unified Role    -> Gemini Handling
SYSTEM          -> Extracted to `systemInstruction` field
DEVELOPER       -> Merged with systemInstruction
USER            -> "user" role
ASSISTANT       -> "model" role
TOOL            -> "user" role with functionResponse parts

ContentPart Translations:
  TEXT          -> { "text": "..." }
  IMAGE (url)  -> { "fileData": { "mimeType": "...", "fileUri": "..." } }
  IMAGE (data) -> { "inlineData": { "mimeType": "...", "data": "<base64>" } }
  TOOL_CALL    -> { "functionCall": { "name": "...", "args": { ... } } }
  TOOL_RESULT  -> { "functionResponse": { "name": "<function_name>", "response": { ... } } }
```

Special behaviors:
- **No developer role:** Treated the same as system.
- **Tool call IDs:** Gemini does not assign unique IDs to function calls. The adapter must generate synthetic unique IDs (e.g., `"call_" + random_uuid()`) and maintain a mapping from synthetic IDs to function names for when tool results are sent back.
- **Function response format:** Gemini's `functionResponse` uses the function *name* (not the call ID) and expects a dict for the response (wrap strings in `{"result": "..."}` if needed).
- **Streaming format:** Gemini uses JSON chunks (optionally via SSE with `?alt=sse`), not a standard SSE endpoint.

### 7.4Tool Definition Translation

| SDK Format              | OpenAI                                             | Anthropic                                        | Gemini                                             |
|-------------------------|----------------------------------------------------|-------------------------------------------------|-----------------------------------------------------|
| Tool.name               | tools[].function.name                              | tools[].name                                     | tools[].functionDeclarations[].name                |
| Tool.description        | tools[].function.description                       | tools[].description                              | tools[].functionDeclarations[].description         |
| Tool.parameters         | tools[].function.parameters                        | tools[].input_schema                             | tools[].functionDeclarations[].parameters          |
| Wrapper structure       | `{"type":"function","function":{...}}`             | `{"name":...,"description":...,"input_schema":...}` | `{"functionDeclarations":[{...}]}`             |

### 7.5 Response Translation

The adapter must parse the provider's response into the unified Response format:

1. **Extract content parts.** Parse the provider's content/parts array into `List<ContentPart>` with appropriate `ContentKind` tags.
2. **Map finish reason.** Translate the provider's finish/stop reason to the unified `FinishReason` (see mapping table in Section 3.8).
3. **Extract usage.** Map the provider's token count fields to `Usage` (see mapping table in Section 3.9).
4. **Preserve raw response.** Store the complete provider response in `Response.raw` for debugging.
5. **Extract rate limit info.** Parse `x-ratelimit-*` headers into `RateLimitInfo` if present.

### 7.6Error Translation

The adapter must translate HTTP errors into the error hierarchy:

1. Parse the response body for error details (message, error code).
2. Extract `Retry-After` header if present.
3. Map the HTTP status code to the appropriate error type using the table in Section 6.4.
4. For ambiguous cases, apply message-based classification (Section 6.5).
5. Preserve the raw error response in the `raw` field.

```
FUNCTION raise_error(http_response):
    body = parse_json(http_response.body)
    message = body.error.message OR http_response.text
    error_code = body.error.code OR body.error.type

    retry_after = None
    IF http_response.headers["retry-after"] EXISTS:
        retry_after = parse_float(http_response.headers["retry-after"])

    RAISE error_from_status_code(
        status_code = http_response.status,
        message = message,
        provider = self.name,
        error_code = error_code,
        raw = body,
        retry_after = retry_after
    )
```

### 7.7 Streaming Translation

The adapter translates provider-specific streaming formats into the unified StreamEvent model.

#### SSE Parsing

Most providers use Server-Sent Events (SSE). A proper SSE parser must handle:

- `event:` lines (event type)
- `data:` lines (payload, may span multiple lines)
- `retry:` lines (reconnection interval)
- Comment lines (starting with `:`)
- Blank lines (event boundary)

The parser yields `(event_type, data)` tuples. Many providers include the event type in the JSON payload as well as in the SSE event field; prefer the JSON payload field for reliability.

#### OpenAI Streaming (Responses API)

The Responses API uses a different streaming format than Chat Completions:

```
Provider Format (Responses API):
    event: response.created        -- response object created
    event: response.in_progress    -- generation started
    event: response.output_text.delta  -- incremental text
    event: response.function_call_arguments.delta  -- incremental tool call args
    event: response.output_item.done   -- output item complete
    event: response.completed      -- generation complete, includes usage with reasoning_tokens

Translation:
    output_text.delta              -> TEXT_DELTA event (emit TEXT_START on first)
    function_call_arguments.delta  -> TOOL_CALL_DELTA event
    output_item.done (text)        -> TEXT_END event
    output_item.done (function)    -> TOOL_CALL_END event
    response.completed             -> FINISH event with usage (including reasoning_tokens)
```

The Responses API streaming format provides reasoning token counts in the final `response.completed` event, which is why it is required for reasoning models.

For the OpenAI-compatible adapter (Chat Completions), the streaming format is:

```
Provider Format (Chat Completions, for third-party endpoints):
    data: {"choices": [{"delta": {"content": "text"}, "finish_reason": null}]}
    data: {"choices": [{"delta": {"tool_calls": [{"index": 0, ...}]}}]}
    data: {"usage": {...}}
    data: [DONE]
```

#### Anthropic Streaming

```
Provider Format (SSE events):
    event: message_start       -- contains message metadata and input token count
    event: content_block_start -- new content block (text, tool_use, thinking)
    event: content_block_delta -- incremental content within a block
    event: content_block_stop  -- block complete
    event: message_delta       -- finish reason and output usage
    event: message_stop        -- stream complete

Translation:
    content_block_start (type=text)     -> TEXT_START
    content_block_delta (type=text)     -> TEXT_DELTA
    content_block_stop  (type=text)     -> TEXT_END
    content_block_start (type=tool_use) -> TOOL_CALL_START
    content_block_delta (type=tool_use) -> TOOL_CALL_DELTA
    content_block_stop  (type=tool_use) -> TOOL_CALL_END
    content_block_start (type=thinking) -> REASONING_START
    content_block_delta (type=thinking) -> REASONING_DELTA
    content_block_stop  (type=thinking) -> REASONING_END
    message_stop                        -> FINISH with accumulated response
```

#### Gemini Streaming

Gemini uses SSE (with `?alt=sse` query parameter) or newline-delimited JSON chunks.

```
Provider Format (SSE):
    data: {"candidates": [{"content": {"parts": [{"text": "..."}]}}], "usageMetadata": {...}}

Translation:
    parts[].text present               -> TEXT_DELTA (emit TEXT_START on first)
    parts[].functionCall present       -> TOOL_CALL_START + TOOL_CALL_END (full call in one chunk)
    candidate.finishReason present     -> TEXT_END
    Final chunk                        -> FINISH with accumulated response
```

Note: Gemini typically delivers function calls as complete objects in a single chunk, not incrementally. Emit both TOOL_CALL_START and TOOL_CALL_END for each function call.

### 7.8 Provider Quirks Reference

A summary of provider-specific behaviors that adapters must handle:

| Concern                      | OpenAI                           | Anthropic                              | Gemini                              |
|------------------------------|----------------------------------|----------------------------------------|-------------------------------------|
| **Native API**               | **Responses API** (`/v1/responses`) | **Messages API** (`/v1/messages`)   | **Gemini API** (`/v1beta/...generateContent`) |
| System message handling      | `instructions` parameter         | Extracted to `system` parameter        | Extracted to `systemInstruction`    |
| Developer role               | `instructions` or `developer` role | Merged with system                   | Merged with system                  |
| Message alternation          | No strict requirement            | Strict user/assistant alternation      | No strict requirement               |
| Reasoning tokens             | Via `output_tokens_details`; requires Responses API | Via thinking blocks (text visible) | Via `thoughtsTokenCount`          |
| Tool call IDs                | Provider-assigned unique IDs     | Provider-assigned unique IDs           | No unique IDs (use function name)   |
| Tool result format           | Separate `tool` role messages    | `tool_result` blocks in user messages  | `functionResponse` in user content  |
| Tool choice "none"           | `"none"`                         | Omit tools from request entirely       | `"NONE"`                            |
| max_tokens                   | Optional                         | Required (default to 4096)             | Optional (as `maxOutputTokens`)     |
| Thinking blocks              | Not exposed (o-series internal)  | `thinking` / `redacted_thinking` blocks| `thought` parts (2.5 models)       |
| Structured output            | Native json_schema mode          | Prompt engineering or tool extraction  | Native responseSchema               |
| Streaming protocol           | SSE with `data:` lines           | SSE with event type + data lines       | SSE (with `?alt=sse`) or JSON       |
| Stream termination           | `data: [DONE]`                   | `message_stop` event                   | Final chunk (no explicit signal)    |
| Finish reason for tools      | `tool_calls`                     | `tool_use`                             | No dedicated reason (infer from parts)|
| Image input                  | Data URI in `image_url`          | `base64` source with `media_type`      | `inlineData` with `mimeType`        |
| Prompt caching               | Automatic (free, 50% discount)   | Requires explicit `cache_control` blocks (90% discount) | Automatic (free prefix caching)   |
| Beta/feature headers         | N/A (features in request body)   | `anthropic-beta` header (comma-separated) | N/A (features in request body)   |
| Authentication               | Bearer token in Authorization    | `x-api-key` header                     | `key` query parameter               |
| API versioning               | Via URL path (/v1/)              | `anthropic-version` header             | Via URL path (/v1beta/)             |

### 7.9 Adding a New Provider

To add support for a new provider:

1. **Implement the ProviderAdapter interface.** Create a class with `name`, `complete()`, and `stream()`.
2. **Write request translation.** Map the unified Request to the provider's API format, following the patterns in Section 7.3.
3. **Write response translation.** Map the provider's response to the unified Response, following Section 7.5.
4. **Write error translation.** Map HTTP errors to the error hierarchy, following Section 7.6.
5. **Write streaming translation.** Map the provider's streaming format to StreamEvent objects, following Section 7.7.
6. **Handle provider quirks.** Document any provider-specific behaviors (like Anthropic's strict alternation or Gemini's missing tool call IDs) and handle them in the adapter.
7. **Register the adapter.** Add it to `Client.from_env()` with the appropriate environment variable checks, or allow users to register it programmatically.

### 7.10OpenAI-Compatible Endpoints

Many third-party services (vLLM, Ollama, Together AI, Groq, etc.) expose an OpenAI-compatible Chat Completions API. For these services, provide a separate `OpenAICompatibleAdapter` that uses the Chat Completions endpoint (`/v1/chat/completions`) rather than the Responses API:

```
adapter = OpenAICompatibleAdapter(
    api_key = "...",
    base_url = "https://my-vllm-instance.example.com/v1"
)
```

This adapter is distinct from the primary OpenAI adapter (which uses the Responses API) because third-party services typically only implement the Chat Completions protocol. The compatible adapter does not support reasoning tokens, built-in tools, or other Responses API features.

---

## Appendix A: Conversation Examples

### A.1 Simple Text Conversation

```
messages = [
    Message(role = SYSTEM, content = [ContentPart(kind = TEXT, text = "You are a helpful assistant.")]),
    Message(role = USER,   content = [ContentPart(kind = TEXT, text = "What is 2 + 2?")])
]
```

### A.2 Multimodal Conversation

```
messages = [
    Message(role = USER, content = [
        ContentPart(kind = TEXT, text = "What do you see in this image?"),
        ContentPart(kind = IMAGE, image = ImageData(url = "https://example.com/photo.jpg"))
    ])
]
```

### A.3 Tool Use Conversation

```
messages = [
    Message(role = USER, content = [
        ContentPart(kind = TEXT, text = "What is the weather in San Francisco?")
    ]),
    Message(role = ASSISTANT, content = [
        ContentPart(kind = TOOL_CALL, tool_call = ToolCallData(
            id = "call_123",
            name = "get_weather",
            arguments = { "city": "San Francisco" }
        ))
    ]),
    Message(role = TOOL, content = [
        ContentPart(kind = TOOL_RESULT, tool_result = ToolResultData(
            tool_call_id = "call_123",
            content = "72F, sunny",
            is_error = false
        ))
    ], tool_call_id = "call_123"),
    Message(role = ASSISTANT, content = [
        ContentPart(kind = TEXT, text = "The weather in San Francisco is 72F and sunny.")
    ])
]
```

### A.4 Thinking Blocks (Anthropic Extended Thinking)

```
messages = [
    Message(role = USER, content = [
        ContentPart(kind = TEXT, text = "Solve this complex math problem...")
    ]),
    Message(role = ASSISTANT, content = [
        ContentPart(kind = THINKING, thinking = ThinkingData(
            text = "Let me work through this step by step...",
            signature = "sig_abc123"
        )),
        ContentPart(kind = TEXT, text = "The answer is 42.")
    ])
]
```

When continuing a conversation that includes thinking blocks, the thinking content parts must be included in the message history so the provider can verify their integrity.

---

## Appendix B: High-Level API Usage Examples

### B.1 Simple Generation

```
result = generate(model = "claude-opus-4-6", prompt = "Explain quantum computing")
PRINT(result.text)
PRINT(result.usage.total_tokens)
```

### B.2 Generation with Tools

```
result = generate(
    model = "claude-opus-4-6",
    system = "You are a helpful assistant with access to weather data.",
    prompt = "What is the weather in San Francisco?",
    tools = [weather_tool],
    max_tool_rounds = 5
)

PRINT(result.text)                              -- final text after all tool rounds
PRINT(LENGTH(result.steps))                     -- number of steps taken
PRINT(result.total_usage.total_tokens)          -- aggregated token count
```

### B.3 Streaming

```
result = stream(model = "claude-opus-4-6", prompt = "Write a poem")

FOR EACH event IN result:
    IF event.type == TEXT_DELTA:
        PRINT(event.delta)

response = result.response()
PRINT(response.usage)
```

### B.4 Structured Output

```
result = generate_object(
    model = "gpt-5.2",
    prompt = "Extract the person's name and age from: 'Alice is 30 years old'",
    schema = {
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" }
        },
        "required": ["name", "age"]
    }
)

PRINT(result.output)    -- { "name": "Alice", "age": 30 }
```

### B.5 Provider Fallback Pattern

```
TRY:
    result = generate(model = "claude-opus-4-6", prompt = "...")
CATCH ProviderError:
    result = generate(model = "gpt-5.2", provider = "openai", prompt = "...")
```

### B.6 Middleware for Logging

```
FUNCTION logging_middleware(request, next):
    start_time = NOW()
    LOG_INFO("LLM request: provider=" + request.provider + " model=" + request.model)
    response = next(request)
    elapsed = NOW() - start_time
    LOG_INFO("LLM response: tokens=" + response.usage.total_tokens + " latency=" + elapsed)
    RETURN response

client = Client(
    providers = { "anthropic": AnthropicAdapter(...) },
    middleware = [logging_middleware]
)
```

---

## Appendix C: Design Decision Rationale

This appendix summarizes key design decisions and the reasoning behind them. These are provided so that implementors understand the "why" and can make informed tradeoffs if their language or context demands different choices.

**Why a single Request type instead of per-method parameter lists?** A single Request object is easier to construct, pass around, modify, and serialize than many keyword arguments. It enables middleware to inspect and modify requests uniformly. High-level functions like `generate(model=..., prompt=...)` provide ergonomic shorthand.

**Why ship a model catalog if model strings work as-is?** Model strings work for developers who know which models exist. But AI coding agents building on top of this SDK often hallucinate model identifiers from stale training data. The catalog gives them a reliable, up-to-date source of valid model IDs and capabilities. Unknown model strings still pass through -- the catalog is advisory, not restrictive.

**Why explicit provider on Request instead of model-based routing?** Several providers serve models with overlapping names. Explicit routing avoids ambiguity. For the common case, `default_provider` removes boilerplate.

**Why separate generate() and stream()?** The return types are fundamentally different: GenerateResult vs StreamResult. A boolean flag loses type safety.

**Why start/delta/end events instead of flat deltas?** Flat deltas lose structural information when a response contains multiple text segments or interleaved tool calls. The pattern adds minimal overhead but enables correct handling of complex responses.

**Why max_tool_rounds instead of unlimited looping?** Unbounded loops risk infinite cycles. A default of 1 is safe. Higher values are an explicit opt-in.

**Why JSON Schema for tool parameters instead of language-native types?** JSON Schema is the universal parameter description format across all providers. Language-native helpers can generate JSON Schema, but JSON Schema is the canonical format.

**Why send error results to the model instead of raising exceptions?** Raising on tool failure aborts the entire generation. Sending an error result gives the model the opportunity to retry, use a different tool, or explain the failure.

**Why default to retrying unknown errors?** Transient failures are more common than permanent ones from unexpected codes. A false retry is cheaper than a false abort.

**Why not retry timed-out requests by default?** Timeouts indicate the operation is inherently slow, not that it failed transiently. Applications can opt in to timeout retries.

**Why use each provider's native API instead of just targeting Chat Completions everywhere?** The Chat Completions API is an OpenAI-specific protocol that other providers partially mimic as a convenience shim. Using it as the universal transport loses critical capabilities: OpenAI's own Responses API exposes reasoning tokens that Chat Completions hides; Anthropic's Messages API supports thinking blocks, prompt caching, and beta headers; Gemini's native API supports grounding and code execution. The unified SDK's value is precisely in abstracting over these different native APIs so callers don't have to. Using a compatibility layer would defeat the purpose.

**Why handle parallel tool execution in the SDK instead of leaving it to the caller?** When a model returns 5 parallel tool calls, the correct behavior is to execute all 5 concurrently, wait for all to complete, and send all 5 results back in one continuation. This is fiddly to implement correctly (error handling, ordering, timeout management) and identical for every consumer. Doing it once in the SDK means coding agents and other downstream tools get it for free.

---

## 8. Definition of Done

This section defines how to validate that an implementation of this spec is complete and correct. Use this as a checklist during development. An implementation is considered done when every item is checked off.

### 8.1 Core Infrastructure

- [ ] `Client` can be constructed from environment variables (`Client.from_env()`)
- [ ] `Client` can be constructed programmatically with explicit adapter instances
- [ ] Provider routing works: requests are dispatched to the correct adapter based on `provider` field
- [ ] Default provider is used when `provider` is omitted from a request
- [ ] `ConfigurationError` is raised when no provider is configured and no default is set
- [ ] Middleware chain executes in correct order (request: registration order, response: reverse order)
- [ ] Module-level default client works (`set_default_client()` and implicit lazy initialization)
- [ ] Model catalog is populated with current models and `get_model_info()` / `list_models()` return correct data

### 8.2 Provider Adapters

For EACH provider (OpenAI, Anthropic, Gemini), verify:

- [ ] Adapter uses the provider's **native API** (OpenAI: Responses API, Anthropic: Messages API, Gemini: Gemini API) -- NOT a compatibility shim
- [ ] Authentication works (API key from env var or explicit config)
- [ ] `complete()` sends a request and returns a correctly populated `Response`
- [ ] `stream()` returns an async iterator of correctly typed `StreamEvent` objects
- [ ] System messages are extracted/handled per provider convention
- [ ] All 5 roles (SYSTEM, USER, ASSISTANT, TOOL, DEVELOPER) are translated correctly
- [ ] `provider_options` escape hatch passes through provider-specific parameters
- [ ] Beta headers are supported (especially Anthropic's `anthropic-beta` header)
- [ ] HTTP errors are translated to the correct error hierarchy types
- [ ] `Retry-After` headers are parsed and set on the error object

### 8.3 Message & Content Model

- [ ] Messages with text-only content work across all providers
- [ ] **Image input works**: images sent as URL, base64 data, and local file path are correctly translated per provider
- [ ] Audio and document content parts are handled (or gracefully rejected if provider doesn't support them)
- [ ] Tool call content parts round-trip correctly (assistant message with tool calls -> tool result messages -> next assistant message)
- [ ] Thinking blocks (Anthropic) are preserved and round-tripped with signatures intact
- [ ] Redacted thinking blocks are passed through verbatim
- [ ] Multimodal messages (text + images in the same message) work

### 8.4 Generation

- [ ] `generate()` works with a simple text `prompt`
- [ ] `generate()` works with a full `messages` list
- [ ] `generate()` rejects when both `prompt` and `messages` are provided
- [ ] `stream()` yields `TEXT_DELTA` events that concatenate to the full response text
- [ ] `stream()` yields `STREAM_START` and `FINISH` events with correct metadata
- [ ] Streaming follows the start/delta/end pattern for text segments
- [ ] `generate_object()` returns parsed, validated structured output
- [ ] `generate_object()` raises `NoObjectGeneratedError` on parse/validation failure
- [ ] Cancellation via abort signal works for both `generate()` and `stream()`
- [ ] Timeouts work (total timeout and per-step timeout)

### 8.5 Reasoning Tokens

- [ ] OpenAI reasoning models (GPT-5.2 series, etc.) return `reasoning_tokens` in `Usage` via the Responses API
- [ ] `reasoning_effort` parameter is passed through correctly to OpenAI reasoning models
- [ ] Anthropic extended thinking blocks are returned as `THINKING` content parts when enabled
- [ ] Thinking block `signature` field is preserved for round-tripping
- [ ] Gemini thinking tokens (`thoughtsTokenCount`) are mapped to `reasoning_tokens` in `Usage`
- [ ] `Usage` correctly reports `reasoning_tokens` as distinct from `output_tokens`

### 8.6 Prompt Caching

- [ ] **OpenAI**: caching works automatically via the Responses API (no client-side configuration needed)
- [ ] **OpenAI**: `Usage.cache_read_tokens` is populated from `usage.prompt_tokens_details.cached_tokens`
- [ ] **Anthropic**: adapter automatically injects `cache_control` breakpoints on the system prompt, tool definitions, and conversation prefix
- [ ] **Anthropic**: `prompt-caching-2024-07-31` beta header is included automatically when cache_control is present
- [ ] **Anthropic**: `Usage.cache_read_tokens` and `Usage.cache_write_tokens` are populated correctly
- [ ] **Anthropic**: automatic caching can be disabled via `provider_options.anthropic.auto_cache = false`
- [ ] **Gemini**: automatic prefix caching works (no client-side configuration needed)
- [ ] **Gemini**: `Usage.cache_read_tokens` is populated from `usageMetadata.cachedContentTokenCount`
- [ ] Multi-turn agentic session: verify that turn 5+ shows significant cache_read_tokens (>50% of input tokens) for all three providers

### 8.7 Tool Calling

- [ ] Tools with `execute` handlers (active tools) trigger automatic tool execution loops
- [ ] Tools without `execute` handlers (passive tools) return tool calls to the caller without looping
- [ ] `max_tool_rounds` is respected: loop stops after the configured number of rounds
- [ ] `max_tool_rounds = 0` disables automatic execution entirely
- [ ] **Parallel tool calls**: when the model returns N tool calls in one response, all N are executed concurrently
- [ ] **Parallel tool results**: all N results are sent back in a single continuation request (not one at a time)
- [ ] Tool execution errors are sent to the model as error results (`is_error = true`), not raised as exceptions
- [ ] Unknown tool calls (model calls a tool not in definitions) send an error result, not an exception
- [ ] `ToolChoice` modes (auto, none, required, named) are translated correctly per provider
- [ ] Tool call argument JSON is parsed and validated before passing to execute handlers
- [ ] `StepResult` objects track each step's tool calls, results, and usage

### 8.8 Error Handling & Retry

- [ ] All errors in the hierarchy are raised for the correct HTTP status codes (see Section 6.4 table)
- [ ] `retryable` flag is set correctly on each error type
- [ ] Exponential backoff with jitter works: delays increase correctly per attempt
- [ ] `Retry-After` header overrides calculated backoff when present (and within `max_delay`)
- [ ] `max_retries = 0` disables automatic retries
- [ ] Rate limit errors (429) are retried transparently
- [ ] Non-retryable errors (401, 403, 404) are raised immediately without retry
- [ ] Retries apply per-step, not to the entire multi-step operation
- [ ] Streaming does not retry after partial data has been delivered

### 8.9 Cross-Provider Parity

Run this validation matrix -- each cell must pass:

| Test Case                                | OpenAI | Anthropic | Gemini |
|------------------------------------------|--------|-----------|--------|
| Simple text generation                   | [ ]    | [ ]       | [ ]    |
| Streaming text generation                | [ ]    | [ ]       | [ ]    |
| Image input (base64)                     | [ ]    | [ ]       | [ ]    |
| Image input (URL)                        | [ ]    | [ ]       | [ ]    |
| Single tool call + execution             | [ ]    | [ ]       | [ ]    |
| Multiple parallel tool calls             | [ ]    | [ ]       | [ ]    |
| Multi-step tool loop (3+ rounds)         | [ ]    | [ ]       | [ ]    |
| Streaming with tool calls                | [ ]    | [ ]       | [ ]    |
| Structured output (generate_object)      | [ ]    | [ ]       | [ ]    |
| Reasoning/thinking token reporting       | [ ]    | [ ]       | [ ]    |
| Error handling (invalid API key -> 401)  | [ ]    | [ ]       | [ ]    |
| Error handling (rate limit -> 429)       | [ ]    | [ ]       | [ ]    |
| Usage token counts are accurate          | [ ]    | [ ]       | [ ]    |
| Prompt caching (cache_read_tokens > 0 on turn 2+) | [ ] | [ ]  | [ ]    |
| Provider-specific options pass through   | [ ]    | [ ]       | [ ]    |

### 8.10 Integration Smoke Test

The ultimate validation: run this end-to-end test against all three providers with real API keys.

```
-- 1. Basic generation across all providers
FOR EACH provider IN ["anthropic", "openai", "gemini"]:
    result = generate(
        model = get_latest_model(provider).id,
        prompt = "Say hello in one sentence.",
        max_tokens = 100,
        provider = provider
    )
    ASSERT result.text is not empty
    ASSERT result.usage.input_tokens > 0
    ASSERT result.usage.output_tokens > 0
    ASSERT result.finish_reason.reason == "stop"

-- 2. Streaming
stream_result = stream(model = "claude-opus-4-6", prompt = "Write a haiku.")
text_chunks = []
FOR EACH event IN stream_result:
    IF event.type == TEXT_DELTA:
        text_chunks.APPEND(event.delta)
ASSERT JOIN(text_chunks) == stream_result.response().text

-- 3. Tool calling with parallel execution
result = generate(
    model = "claude-opus-4-6",
    prompt = "What is the weather in San Francisco and New York?",
    tools = [weather_tool],    -- tool that returns mock weather data
    max_tool_rounds = 3
)
ASSERT LENGTH(result.steps) >= 2               -- at least: initial call + after tool results
ASSERT result.text contains "San Francisco"
ASSERT result.text contains "New York"

-- 4. Image input
result = generate(
    model = "claude-opus-4-6",
    messages = [Message(role=USER, content=[
        ContentPart(kind=TEXT, text="What do you see?"),
        ContentPart(kind=IMAGE, image=ImageData(data=<png_bytes>, media_type="image/png"))
    ])]
)
ASSERT result.text is not empty

-- 5. Structured output
result = generate_object(
    model = "gpt-5.2",
    prompt = "Extract: Alice is 30 years old",
    schema = {"type":"object", "properties":{"name":{"type":"string"},"age":{"type":"integer"}}, "required":["name","age"]}
)
ASSERT result.output.name == "Alice"
ASSERT result.output.age == 30

-- 6. Error handling
TRY:
    generate(model = "nonexistent-model-xyz", prompt = "test", provider = "openai")
    FAIL("Should have raised an error")
CATCH NotFoundError:
    PASS   -- correct error type
```

If all items in this section are checked off, the unified LLM library is complete and ready for use as the foundation for a coding agent or any other LLM-powered application.
