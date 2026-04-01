# Async Rust Fake OpenAI Test Plan

The approved testing strategy still holds after reconciling it with the implementation plan. The plan narrows the generic strategy into a concrete async Rust action surface: `GET /healthz`, `POST /v1/responses`, `POST /v1/chat/completions`, and the unauthenticated admin control routes `POST /__admin/scenarios`, `POST /__admin/reset`, and `GET /__admin/requests`. No strategy change requiring user approval was identified.

## Harness requirements

1. **Ephemeral server harness**
   - What it does: boots the real `tokio` + `axum` server on an ephemeral localhost port with test config, then drives it through real HTTP.
   - What it exposes: base URL, authenticated and unauthenticated HTTP clients, SSE reader utilities, timeout helpers, and optional raw socket/body readers for truncated-stream cases.
   - Estimated complexity: medium.
   - Tests depending on it: 1 through 14.

2. **Admin scenario harness**
   - What it does: scripts deterministic server behavior through `POST /__admin/scenarios`, clears state with `POST /__admin/reset`, and fetches normalized request logs from `GET /__admin/requests`.
   - What it exposes: helpers to enqueue FIFO scenarios, seed failure scripts, reset state between tests, and fetch request-log artifacts for assertions.
   - Estimated complexity: medium.
   - Tests depending on it: 4 through 13.

3. **Canonical output comparison helpers**
   - What it does: normalizes non-stream JSON and streamed SSE transcripts into comparable observable artifacts so the same canonical plan can be validated across transport modes and endpoints.
   - What it exposes: parsed text transcript, tool-call transcript, reasoning transcript, completion marker presence, and required-field assertions.
   - Estimated complexity: low to medium.
   - Tests depending on it: 3, 6, 8, 9.

4. **Optional live OpenAI differential harness**
   - What it does: when explicit credentials are present outside normal CI, sends normalized requests to both `twin-openai` and the real OpenAI API and compares protocol shape rather than literal generated text.
   - What it exposes: paired request runner, field-by-field comparator for required JSON fields and SSE event ordering, and opt-in skip behavior when credentials are absent.
   - Estimated complexity: medium.
   - Tests depending on it: 12.

## Test plan

1. **Name**: health and auth endpoints enforce the public service boundary
   - **Type**: integration
   - **Disposition**: new
   - **Harness**: Ephemeral server harness
   - **Preconditions**: server is running with default local-test config and no scenarios loaded.
   - **Actions**: `GET /healthz`; `POST /v1/responses` without `Authorization`; `POST /v1/chat/completions` with an empty bearer token; `POST /v1/responses` with a non-empty bearer token and minimal valid JSON.
   - **Expected outcome**: `GET /healthz` returns `200`; `/v1/*` rejects missing or empty bearer auth with stable OpenAI-shaped error JSON; a syntactically valid authenticated request reaches endpoint handling rather than failing auth. Source of truth: approved strategy, implementation plan scope and product contract.
   - **Interactions**: router wiring, auth middleware, request parsing, error serialization.

2. **Name**: non-stream responses create returns deterministic OpenAI-shaped JSON
   - **Type**: integration
   - **Disposition**: new
   - **Harness**: Ephemeral server harness
   - **Preconditions**: server is running, no scenario matches the request.
   - **Actions**: `POST /v1/responses` with bearer auth, `stream=false`, text input, and optional inert `OpenAI-Organization` and `OpenAI-Project` headers.
   - **Expected outcome**: `200` with valid `/v1/responses` JSON containing deterministic ID format, timestamp, output items, and usage object; response text is the documented deterministic fallback derived from user text rather than a real model output. Source of truth: implementation plan user-visible behavior, required invariants, OpenAI Responses API shape.
   - **Interactions**: request models, deterministic default engine, JSON serialization, inert compatibility header handling.

3. **Name**: streaming responses emit valid SSE and the same content as non-stream responses
   - **Type**: invariant
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Canonical output comparison helpers
   - **Preconditions**: server is running, no scenario matches the request, same logical request body is available in both stream and non-stream forms.
   - **Actions**: send one `POST /v1/responses` with `stream=false` and one with `stream=true`; collect the full JSON body and full SSE transcript.
   - **Expected outcome**: streamed events are valid SSE, ordered, flushed incrementally, and terminate with the supported completion semantics; the user-visible text, reasoning content, and tool-call transcript reconstructed from the stream match the non-stream response derived from the same canonical plan. Source of truth: approved strategy, implementation plan invariants, OpenAI streaming docs.
   - **Interactions**: canonical response-plan projection, SSE encoder, chunk flushing, completion signaling.

4. **Name**: admin-loaded scenarios are consumed once and in FIFO order
   - **Type**: scenario
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness
   - **Preconditions**: server is running; two matching scenarios are loaded for the same endpoint and matcher.
   - **Actions**: `POST /__admin/scenarios` with two matching scripts; call the matching OpenAI endpoint twice; call it a third time after the queue is exhausted; fetch `GET /__admin/requests`; reset via `POST /__admin/reset`.
   - **Expected outcome**: first OpenAI request consumes the first script, second consumes the second, third falls back to deterministic default behavior; request log is append-only until reset and empty after reset. Source of truth: implementation plan scenario scripting model, user-visible behavior, required invariants.
   - **Interactions**: admin API, scenario matcher, FIFO consumption, fallback engine, request logging, reset behavior.

5. **Name**: responses accept the declared compatibility fields and reject unsupported combinations clearly
   - **Type**: boundary
   - **Disposition**: new
   - **Harness**: Ephemeral server harness
   - **Preconditions**: server is running with no scenarios required.
   - **Actions**: `POST /v1/responses` requests covering `metadata`, `stop`, `previous_response_id`, reasoning options, image inputs, `text` response format, `json_object`, supported `json_schema`, and one unsupported field or unsupported schema construct.
   - **Expected outcome**: supported fields are accepted without changing the documented deterministic semantics; unsupported combinations fail with stable OpenAI-shaped error JSON rather than silent success. Source of truth: implementation plan endpoint compatibility contract, approved strategy, OpenAI docs for supported fields.
   - **Interactions**: request validation, schema subset validation, image-input parsing, explicit unsupported-case handling.

6. **Name**: scripted tool-call and continuation flows work through responses in stream and non-stream modes
   - **Type**: scenario
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness plus Canonical output comparison helpers
   - **Preconditions**: server is running; an admin scenario is queued that emits a tool call on the first request and a final answer on a continuation request that includes `function_call_output`.
   - **Actions**: load the scripted scenario; call `POST /v1/responses` once non-stream and once stream to observe tool-call emission; call `POST /v1/responses` again with continuation input containing the tool output and optional `previous_response_id`.
   - **Expected outcome**: first turn returns or streams a valid tool-call item with deterministic IDs and supported reasoning content; continuation request is accepted and returns the scripted final answer; stream and non-stream transcripts stay equivalent for the same planned turn. Source of truth: implementation plan tool-call and continuation contract, OpenAI function-calling and Responses docs.
   - **Interactions**: scenario engine, canonical plan, responses renderer, continuation input parsing, SSE event sequencing.

7. **Name**: structured output support is explicit, deterministic, and bounded to the documented schema subset
   - **Type**: boundary
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness where needed
   - **Preconditions**: server is running; supported and unsupported response format payloads are available.
   - **Actions**: call `POST /v1/responses` with `json_object`; call with a supported `json_schema` subset; call with an unsupported schema feature such as an out-of-scope construct defined by the compatibility matrix.
   - **Expected outcome**: `json_object` and supported `json_schema` requests return deterministic JSON matching the declared format; unsupported schema constructs fail explicitly with stable error JSON. Source of truth: implementation plan deterministic structured-output behavior and compatibility-matrix requirement, OpenAI structured outputs docs.
   - **Interactions**: response-format validation, deterministic JSON generation, error-body rendering.

8. **Name**: chat completions non-stream and stream use the same canonical plan as responses when behavior is equivalent
   - **Type**: invariant
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness plus Canonical output comparison helpers
   - **Preconditions**: server is running; one default request pair and one scripted scenario pair exist that are semantically representable on both endpoints.
   - **Actions**: send equivalent requests to `POST /v1/responses` and `POST /v1/chat/completions` in both stream and non-stream forms.
   - **Expected outcome**: both endpoints produce their respective OpenAI-shaped envelopes, but the user-visible text, reasoning transcript, and tool-call intent are equivalent because both render the same canonical plan; chat streaming emits valid delta events and terminal markers. Source of truth: implementation plan required invariants and endpoint compatibility contract, OpenAI Responses and Chat Completions docs.
   - **Interactions**: cross-endpoint adapters, canonical plan projection, chat delta SSE rendering, shared validation helpers.

9. **Name**: chat completions accept supported fields and reject unsupported combinations explicitly
   - **Type**: boundary
   - **Disposition**: new
   - **Harness**: Ephemeral server harness
   - **Preconditions**: server is running with no scenario required.
   - **Actions**: `POST /v1/chat/completions` with supported `tools`, `tool_choice`, `response_format`, `stop`, and reasoning-bearing assistant content; send one request with an unsupported combination declared outside the compatibility matrix.
   - **Expected outcome**: supported requests succeed in stream and non-stream modes; unsupported combinations fail with stable OpenAI-shaped error JSON rather than being ignored. Source of truth: implementation plan endpoint compatibility contract, OpenAI Chat Completions docs.
   - **Interactions**: chat request parsing, response-format handling, tool-choice validation, error serialization.

10. **Name**: scripted application errors preserve status, error shape, and retry metadata
   - **Type**: integration
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness
   - **Preconditions**: server is running; scenarios are loaded for each scripted application error variant.
   - **Actions**: queue and invoke scripted failures for `400`, `401`, `403`, `404`, `408`, `413`, `429`, `500`, `502`, `503`, and `504`; include distinct quota-style and content-filter-style error bodies and a `429` carrying `Retry-After`.
   - **Expected outcome**: each request returns the scripted HTTP status, OpenAI-shaped error JSON, and any scripted `Retry-After` header without transport corruption; quota-style and content-filter-style bodies remain observably distinct from generic invalid-request errors. Source of truth: approved strategy failure matrix, implementation plan task 7.
   - **Interactions**: admin scripting, error renderer, header propagation, status mapping.

11. **Name**: delayed first byte and hung requests are observable over real sockets and bounded by client timeouts
   - **Type**: scenario
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness
   - **Preconditions**: server is running; one scenario delays before headers and one hangs forever.
   - **Actions**: invoke the delayed-first-byte scenario with a timeout larger than the scripted delay; invoke the hang scenario with a short explicit client timeout.
   - **Expected outcome**: delayed-first-byte requests eventually succeed after the scripted pause; hung requests never produce completion and are terminated by the test client timeout rather than silently succeeding or closing early. Source of truth: approved strategy performance and failure-injection requirements, implementation plan transport-failure mechanics.
   - **Interactions**: async timing, response-body start behavior, timeout guards, scenario engine.

12. **Name**: partial stream close and malformed SSE are distinguishable transport failures
   - **Type**: regression
   - **Disposition**: new
   - **Harness**: Ephemeral server harness plus Admin scenario harness with raw socket/body reader support
   - **Preconditions**: server is running; one scenario is configured to close after N chunks and another to emit malformed or truncated SSE.
   - **Actions**: invoke both scenarios against streaming `/v1/responses` and streaming `/v1/chat/completions`; capture raw stream bytes and parsed client behavior.
   - **Expected outcome**: the partial-close case yields a valid prefix of the transcript followed by observable premature termination; the malformed/truncated case yields an invalid SSE/body artifact rather than a well-formed completion. Both failures are visible at the HTTP/SSE surface, not only through internal state. Source of truth: approved strategy failure matrix, implementation plan task 7 and execution notes.
   - **Interactions**: streaming body writer, SSE encoder, abrupt connection teardown, lower-level HTTP client behavior.

13. **Name**: optional live OpenAI differential checks preserve required protocol shape for the supported matrix
   - **Type**: differential
   - **Disposition**: new
   - **Harness**: Optional live OpenAI differential harness
   - **Preconditions**: explicit real OpenAI credentials are available outside required CI; normalized request fixtures exist only for the documented supported matrix.
   - **Actions**: send paired non-stream and stream requests for supported `responses` and `chat.completions` cases to `twin-openai` and the real OpenAI API; compare status classes, required fields, SSE event ordering, and header behavior while ignoring literal text.
   - **Expected outcome**: for the documented supported matrix, `twin-openai` matches the real API on protocol shape closely enough that generic OpenAI clients can interact with it; any intentional deviations are recorded in the compatibility matrix rather than hidden. Source of truth: approved strategy reference-comparison harness, official OpenAI API docs, real API as reference implementation.
   - **Interactions**: external OpenAI service, local compatibility matrix, normalization/comparison utilities.

14. **Name**: localhost success paths stay fast enough to catch accidental blocking or deadlock
   - **Type**: invariant
   - **Disposition**: new
   - **Harness**: Ephemeral server harness
   - **Preconditions**: server is running locally with default deterministic behavior and no injected delays.
   - **Actions**: time a representative non-stream `/v1/responses` request, a representative streaming `/v1/responses` request until first event and completion, and a representative `/v1/chat/completions` request.
   - **Expected outcome**: success-path requests complete comfortably under the generous local thresholds from the approved strategy, and first stream event arrives quickly enough to catch catastrophic async blocking rather than normal variance. Source of truth: approved strategy performance section.
   - **Interactions**: full request path, async scheduling, streaming flush behavior, serialization.

## Coverage summary

Covered action space:
- `GET /healthz`
- `POST /v1/responses` with and without auth
- `POST /v1/responses` non-stream success
- `POST /v1/responses` stream success
- `POST /v1/responses` with `metadata`
- `POST /v1/responses` with `stop`
- `POST /v1/responses` with `previous_response_id`
- `POST /v1/responses` with reasoning options
- `POST /v1/responses` with image inputs
- `POST /v1/responses` with `json_object`
- `POST /v1/responses` with supported `json_schema`
- `POST /v1/responses` with unsupported schema or unsupported field combinations
- `POST /v1/responses` scripted tool-call turn
- `POST /v1/responses` continuation turn with `function_call_output`
- `POST /v1/chat/completions` with and without auth
- `POST /v1/chat/completions` non-stream success
- `POST /v1/chat/completions` stream success
- `POST /v1/chat/completions` with `tools`
- `POST /v1/chat/completions` with `tool_choice`
- `POST /v1/chat/completions` with `response_format`
- `POST /v1/chat/completions` with `stop`
- `POST /v1/chat/completions` with reasoning-bearing assistant content
- `POST /v1/chat/completions` with unsupported field combinations
- `POST /__admin/scenarios`
- `POST /__admin/reset`
- `GET /__admin/requests`
- Scripted status-error variants
- Scripted quota/content-filter variants
- Scripted `Retry-After`
- Scripted delayed first byte
- Scripted hang forever
- Scripted partial stream then close
- Scripted malformed or truncated SSE

Explicit exclusions per the agreed strategy:
- Any downstream-consumer black-box tests, fixtures, scripts, CI jobs, or docs.
- Any OpenAI endpoint outside the documented supported phase-one matrix.
- Undocumented compatibility quirks not captured by the official docs or the optional live differential suite.
- Production-scale performance benchmarking; only generous local guardrail timing assertions are included.

Risk carried by exclusions:
- Generic client compatibility outside the documented matrix may still drift until exercised by the optional differential suite.
- Consumers relying on undocumented OpenAI edge behavior may discover gaps that this repository intentionally does not claim to support.
