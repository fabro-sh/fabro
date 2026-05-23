# LLM Input Token Counting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional LLM adapter capability that returns current input/context token counts, using provider-native counting when available and a deterministic local estimate otherwise.

**Architecture:** Token counting belongs in `fabro-llm` because each provider adapter owns the final provider-specific request serialization. `Client::count_input_tokens` will resolve and validate the request through the same provider path as `complete` and `stream`, try the adapter count API when requested, and fall back to a local estimate only for explicitly fallback-eligible failures. The returned value reports input/context size only, not billing usage.

**Tech Stack:** Rust, async-trait, serde/serde_json, fabro-http, httpmock, existing `fabro-llm` provider adapters.

---

## Scope And Decisions

- Build the reusable `fabro-llm` capability only. Session-level context breakdown, API endpoints, and UI rendering are follow-up work.
- Count input/context tokens only: model-visible messages, system/developer instructions, tools, tool choice, response schemas, and structured input content.
- Do not reuse `TokenCounts`; it includes output, reasoning, cache-read, and cache-write billing buckets.
- Prefer provider-native counting when callers choose it, but do not hide deterministic configuration, credential, request-shape, model-availability, content-filter, or context-length errors behind a local estimate.
- `PreferProvider` falls back only for unsupported adapters, timeout/network errors, rate limits, and provider 5xx/server errors.
- `RequireProvider` never returns a local estimate. It returns a provider count or an error.
- `EstimateOnly` still resolves and validates the provider/model, but does not call the adapter or send request content upstream.
- Privacy: `PreferProvider` and `RequireProvider` send the model-visible request to the upstream provider's token-count endpoint. That includes messages, system/developer instructions, tools, schemas, structured content, and media metadata/content according to provider serialization. `EstimateOnly` is the privacy-preserving mode.

## File Structure

- Create `lib/crates/fabro-llm/src/token_count.rs`
  - Public token-counting result/preference types.
  - Deterministic local estimator.
  - Unit tests for estimator behavior.
- Modify `lib/crates/fabro-llm/src/lib.rs`
  - Export the new module and public types.
- Modify `lib/crates/fabro-llm/src/provider.rs`
  - Add the optional adapter method with a default unsupported implementation.
- Modify `lib/crates/fabro-llm/src/client.rs`
  - Add `Client::count_input_tokens`.
  - Add tests for fallback and preference behavior.
- Modify provider adapter files under `lib/crates/fabro-llm/src/providers/`
  - Anthropic: count via `/messages/count_tokens`.
  - Gemini: count via `models/{model}:countTokens`.
  - OpenAI: count via `/responses/input_tokens`.
  - OpenAI-compatible and Fabro-server adapters keep the default unsupported path.

## Task 1: Add Public Types And Local Estimator

**Files:**
- Create: `lib/crates/fabro-llm/src/token_count.rs`
- Modify: `lib/crates/fabro-llm/src/lib.rs`

- [ ] Define these public types in `token_count.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::types::{ContentPart, Request, ToolDefinition, Warning};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenCountPreference {
    PreferProvider,
    RequireProvider,
    EstimateOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenCountMethod {
    ProviderApi,
    LocalEstimate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTokenCount {
    pub input_tokens: i64,
    pub method: InputTokenCountMethod,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub warnings: Vec<Warning>,
}
```

- [ ] Add `estimate_input_tokens(request: &Request, provider: impl Into<String>) -> InputTokenCount`.
  - Use `InputTokenCountMethod::LocalEstimate`.
  - Set `model` from `request.model`.
  - Add deterministic warning codes as needed:
    - `local_token_estimate`: every local estimate.
    - `media_token_estimate`: media content was counted by a fixed heuristic or embedded byte estimate.
    - `opaque_context_estimate`: opaque provider-specific context was serialized or approximated without provider semantics.
    - `provider_options_estimate`: provider options may affect model-visible context and were counted by JSON-size heuristic.
  - De-duplicate warnings by code so repeated media or opaque parts do not produce noisy results.

- [ ] Implement deterministic estimator helpers:
  - `estimate_text_tokens(text)`: `text.chars().count().div_ceil(4)`; empty text counts as 0.
  - `estimate_json_tokens(value)`: compact `serde_json::to_string(value)` length rounded up at 4 chars/token.
  - Message overhead: 4 tokens per message plus 1 token per content part.
  - Tool overhead: 8 tokens per tool plus estimated name, description, and schema JSON.
  - Tool choice and response format: estimate their serialized JSON values.
  - `provider_options`: estimate serialized JSON and add `provider_options_estimate`.
  - Images: 2,000 token media floor plus metadata text/URL estimate.
  - Audio/documents: estimate embedded byte length at 4 bytes/token; URL-only media uses a 2,000 token media floor plus metadata.
  - File IDs and URL-only media: count the ID/URL text plus the 2,000 token media floor and add `media_token_estimate`.
  - Embedded media bytes: count byte length divided by 4, rounded up, and add `media_token_estimate`.
  - Gemini cached content options: estimate serialized `provider_options.gemini.cached_content` and add `provider_options_estimate`.
  - `ContentPart::Other`: estimate serialized JSON and add `opaque_context_estimate`.
  - OpenAI opaque previous-response/message/reasoning items in `ContentPart::Other`: estimate serialized JSON and add `opaque_context_estimate`.

- [ ] Export the module and public types from `lib.rs`:

```rust
pub mod token_count;

pub use token_count::{
    InputTokenCount, InputTokenCountMethod, InputTokenCountPreference, estimate_input_tokens,
};
```

- [ ] Add estimator tests in `token_count.rs`:
  - text-only request returns a positive local estimate
  - adding a tool increases the estimate
  - adding response format schema increases the estimate
  - image/document/audio content gets a media warning or media-sized estimate
  - provider options produce `provider_options_estimate`
  - opaque `ContentPart::Other` produces `opaque_context_estimate`
  - estimator is deterministic for the same request

Run:

```bash
cargo nextest run -p fabro-llm token_count
```

Expected: estimator tests pass.

## Task 2: Add Adapter Capability And Client Fallback

**Files:**
- Modify: `lib/crates/fabro-llm/src/provider.rs`
- Modify: `lib/crates/fabro-llm/src/client.rs`

- [ ] Extend `ProviderAdapter` with this default method:

```rust
async fn count_input_tokens(
    &self,
    _request: &Request,
) -> Result<Option<InputTokenCount>, Error> {
    Ok(None)
}
```

- [ ] Add imports in `provider.rs` for `InputTokenCount`.

- [ ] Add `Client::count_input_tokens(&self, request: &Request, preference: InputTokenCountPreference) -> Result<InputTokenCount, Error>`.
  - Call `self.validate_request_controls(request)?`.
  - Resolve provider with `self.resolve_provider(request)?`.
  - Call `provider.validate_request(request)?`.
  - If preference is `EstimateOnly`, return `estimate_input_tokens(request, provider.name())`.
  - If preference is `PreferProvider` or `RequireProvider`, call `provider.count_input_tokens(request).await`.
  - Return provider result when it is `Ok(Some(count))`.
  - In `PreferProvider`, return local estimate with warning code `provider_token_count_unsupported` when it is `Ok(None)`.
  - In `RequireProvider`, return `Err(Error::Configuration { .. })` when it is `Ok(None)`.
  - In `PreferProvider`, fallback to local estimate with warning code `provider_token_count_failed` only when the adapter returns:
    - `Error::Network`
    - `Error::RequestTimeout`
    - `Error::Provider { kind: ProviderErrorKind::RateLimit, .. }`
    - `Error::Provider { kind: ProviderErrorKind::Server, .. }`
  - In `PreferProvider`, return the original error for:
    - provider resolution errors
    - `provider.validate_request` errors
    - provider 400 invalid request / `ProviderErrorKind::InvalidRequest`
    - authentication / access denied
    - not found / model unavailable
    - context-length and content-filter errors
    - quota exceeded and every other provider error kind not explicitly listed as fallback-eligible
    - configuration, unsupported tool choice, interrupt, invalid tool call, no-object, and stream errors
  - In `RequireProvider`, return the original adapter error for every error kind; never fallback.
  - Do not run completion/stream middleware for token counting.

- [ ] Add client tests using a mock adapter:
  - provider result is returned when adapter returns `Ok(Some(_))`
  - `PreferProvider` unsupported adapter returns local estimate with `provider_token_count_unsupported`
  - `RequireProvider` unsupported adapter returns `Err`
  - `PreferProvider` falls back for timeout, network, rate-limit, and server errors
  - `PreferProvider` returns `Err` for invalid request, auth, access denied, not found, context length, content filter, quota exceeded, configuration, and unsupported tool choice errors
  - `RequireProvider` returns `Err` for fallback-eligible provider errors
  - `EstimateOnly` does not call the adapter method
  - validation errors still return `Err`

Run:

```bash
cargo nextest run -p fabro-llm client::tests::count_input_tokens
```

Expected: new client tests pass.

## Task 3: Implement Anthropic Provider Counting

**Files:**
- Modify: `lib/crates/fabro-llm/src/providers/anthropic.rs`

- [ ] Reuse the existing request translation from `build_api_request(adapter, request, false).await` so count requests match normal Anthropic serialization.

- [ ] Add a private count request/response shape:

```rust
#[derive(serde::Serialize)]
struct CountTokensRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct CountTokensResponse {
    input_tokens: i64,
}
```

- [ ] Add `count_tokens_url()` returning `"{base_url}/messages/count_tokens"`.

- [ ] Implement `ProviderAdapter::count_input_tokens` for Anthropic:
  - Build the translated API request.
  - Send only count-supported input fields: `model`, `messages`, `system`, `tools`, `tool_choice`, and `thinking`.
  - Do not send generation-only fields from the normal request body: `max_tokens`, `temperature`, `top_p`, `stop_sequences`, `output_config`, `speed`, `metadata`, or `stream`.
  - Apply the same auth, `anthropic-version`, default headers, and beta headers needed for the translated request.
  - Parse `input_tokens`.
  - Return `InputTokenCount { method: ProviderApi, provider: self.provider_name.clone(), model: request.model.clone(), warnings: vec![] }`.

- [ ] Add `httpmock` tests:
  - request path is `/messages/count_tokens`
  - body includes translated `model`, `messages`, `system`, and `tools`
  - reasoning-effort requests that normally produce `output_config` do not include `output_config` in the count body
  - extended thinking configured through provider options is included as `thinking` when the normal translated request includes it
  - response `{ "input_tokens": 123 }` returns `ProviderApi` with `123`
  - non-2xx provider response surfaces from the adapter so the client fallback test can handle it

Run:

```bash
cargo nextest run -p fabro-llm providers::anthropic::tests::count_input_tokens
```

Expected: Anthropic count tests pass.

## Task 4: Implement Gemini Provider Counting

**Files:**
- Modify: `lib/crates/fabro-llm/src/providers/gemini.rs`

- [ ] Reuse `build_api_request(request).await` to build the normal Gemini `generateContent` body.

- [ ] Add a private response shape:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CountTokensResponse {
    total_tokens: i64,
}
```

- [ ] Implement `ProviderAdapter::count_input_tokens` for Gemini:
  - Resolve API model with `common::api_model_id(self.catalog.as_deref(), &request.model)`.
  - POST to `"{base_url}/models/{api_model}:countTokens"`.
  - Body is `{ "generateContentRequest": api_body }`.
  - Apply `x-goog-api-key` and default headers as in `complete`.
  - Parse `totalTokens`.
  - Return `InputTokenCount` with `ProviderApi`.

- [ ] Add `httpmock` tests:
  - request path is `/models/<model>:countTokens`
  - body nests the generated request under `generateContentRequest`
  - body does not include top-level `contents`, because Gemini requires `contents` and `generateContentRequest` to be mutually exclusive
  - response `{ "totalTokens": 456 }` returns `456`
  - tool declarations and system instructions are included through the reused translator

Run:

```bash
cargo nextest run -p fabro-llm providers::gemini::tests::count_input_tokens
```

Expected: Gemini count tests pass.

## Task 5: Implement OpenAI Provider Counting

**Files:**
- Modify: `lib/crates/fabro-llm/src/providers/openai.rs`

- [ ] Reuse `build_request_body_with_catalog(request, false, self.codex_mode, self.catalog.as_deref()).await`, then filter the body to the `/responses/input_tokens` allow-list.
- [ ] Keep only these top-level fields when present: `conversation`, `input`, `instructions`, `model`, `parallel_tool_calls`, `previous_response_id`, `reasoning`, `text`, `tool_choice`, `tools`, and `truncation`.
- [ ] Strip generation/storage/response-shaping fields from the count request body: `background`, `include`, `max_output_tokens`, `max_tool_calls`, `metadata`, `prompt`, `prompt_cache_key`, `safety_identifier`, `service_tier`, `store`, `stream`, `temperature`, `top_logprobs`, `top_p`, `user`, `stop`, and any Codex-only generated fields not in the allow-list.

- [ ] Add a private response shape matching the current Responses input-token API:

```rust
#[derive(serde::Deserialize)]
struct InputTokensResponse {
    input_tokens: i64,
    object: String,
}
```

- [ ] Implement `ProviderAdapter::count_input_tokens` for OpenAI:
  - POST to `"{base_url}/responses/input_tokens"`.
  - Use `self.build_request(&url).json(&filtered_request_body)` so auth/org/project/default headers match completion behavior.
  - Validate `object == "response.input_tokens"`; otherwise return a network parse error.
  - Parse top-level `input_tokens`.
  - Return `InputTokenCount` with `ProviderApi`.

- [ ] Add `httpmock` tests:
  - request path is `/responses/input_tokens`
  - request body is exactly the allow-listed count body for messages, instructions, tools, reasoning, and response format
  - request body strips `store`, `include`, `stream`, `max_output_tokens`, `metadata`, `temperature`, `top_p`, and `stop`
  - response `{ "object": "response.input_tokens", "input_tokens": 789 }` returns `789`
  - response with the wrong `object` returns an error
  - `codex_mode` count uses the same serialization choices as Codex-mode streaming requests

Run:

```bash
cargo nextest run -p fabro-llm providers::openai::tests::count_input_tokens
```

Expected: OpenAI count tests pass.

## Task 6: Integration, Docs, And Final Verification

**Files:**
- Modify: `lib/crates/fabro-llm/README.md`

- [ ] Add a short README section showing:
  - `client.count_input_tokens(&request, InputTokenCountPreference::PreferProvider).await`
  - `RequireProvider` for callers that need provider semantics and must not accept estimates
  - `EstimateOnly` as the privacy-preserving mode
  - provider fallback behavior
  - privacy/data exposure for provider-native counting: model-visible request content is sent to the provider token-count endpoint
  - the distinction between `InputTokenCount` and billing `TokenCounts`

- [ ] Run formatting check:

```bash
cargo +nightly-2026-04-14 fmt --check --all
```

Expected: passes. If it fails only because new files need formatting, run `cargo +nightly-2026-04-14 fmt --all` and re-check.

- [ ] Run focused tests:

```bash
cargo nextest run -p fabro-llm
```

Expected: all `fabro-llm` tests pass.

- [ ] Run workspace build:

```bash
cargo build --workspace
```

Expected: workspace builds successfully.

## Acceptance Criteria

- Callers can ask the LLM client for input/context token count without sending a completion request.
- Anthropic, Gemini, and OpenAI adapters use provider-native counting endpoints.
- Unsupported adapters and provider count failures return deterministic local estimates only when the failure is explicitly fallback-eligible.
- `RequireProvider` never returns a local estimate.
- Provider count requests are filtered to fields accepted by each provider's count endpoint.
- README documents privacy implications of provider-native counting.
- The result cannot be mistaken for billing totals because it uses new input-token-specific types.
- Existing completion and streaming behavior is unchanged.
