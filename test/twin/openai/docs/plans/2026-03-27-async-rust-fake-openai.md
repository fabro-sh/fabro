# Async Rust Fake OpenAI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a standalone async Rust HTTP service that is OpenAI-compatible for the supported endpoints, deterministic without using a real LLM, and able to script protocol and transport failures for black-box end-to-end testing.

**Architecture:** Implement a single `tokio` + `axum` server with one canonical internal turn/stream model that feeds both `/v1/responses` and `/v1/chat/completions`. Drive behavior through a neutral in-memory scenario engine plus admin control endpoints so success and failure paths are deterministic, consumer-agnostic, and identical across streaming and non-streaming transports.

**Tech Stack:** Rust, Tokio, Axum, Hyper, Serde, Reqwest, futures-util, uuid, tracing, anyhow/thiserror.

---

## Scope and Product Contract

This repository stays consumer-agnostic. It must not mention, import, script against, or test against any downstream application. The product is the fake OpenAI server itself.

The supported steady-state surface should be:

- `POST /v1/responses`
- `POST /v1/chat/completions`
- `GET /healthz`
- `POST /__admin/scenarios`
- `POST /__admin/reset`
- `GET /__admin/requests`

The service should require a bearer token for `/v1/*` routes and ignore its value beyond presence, so generic clients can use any non-empty API key. The admin routes should be intentionally local-test-focused and unauthenticated by default.

Within those two OpenAI endpoints, phase-one compatibility must explicitly cover the request features this fake is expected to handle in practice: bearer auth, optional `OpenAI-Organization` and `OpenAI-Project` headers, `stream`, tools, `tool_choice`, `stop`, `metadata`, `previous_response_id`, reasoning requests, text response formats (`text`, `json_object`, `json_schema` subset), and image inputs. Anything outside that matrix must fail clearly and be documented as unsupported.

### User-visible behavior

- Non-streaming endpoints return valid OpenAI-shaped JSON with deterministic IDs, timestamps, usage objects, and output items.
- Streaming endpoints emit valid SSE with flush-per-event behavior and proper terminal completion semantics for the supported endpoint.
- If a scenario is preloaded through the admin API, the next matching OpenAI request consumes that scenario exactly once and responds according to its script.
- If no scripted scenario matches, the server falls back to a documented deterministic default behavior so the fake remains usable without setup.
- Tool-call and continuation flows are supported through the canonical internal turn model rather than endpoint-specific hacks.
- Failure injection can simulate application errors and transport failures including hangs, delayed first byte, partial stream then close, and malformed/truncated stream bodies.

### Required invariants

- The same internal response plan drives both stream and non-stream rendering for a given request.
- The same internal response plan drives both `/v1/responses` and `/v1/chat/completions` when the underlying behavior is equivalent.
- Scenario matching and consumption are deterministic and thread-safe.
- Request logs are append-only during a test run and resettable through admin control.
- Unsupported request shapes fail explicitly with stable error JSON; they must not silently degrade into a misleading success.
- No code or docs in this repository may reference downstream projects or embed downstream-specific fixtures.

## Strategy Gate

The clean path is to treat this as a server product, not a pile of endpoint stubs. The central design decision is to introduce a canonical internal "response plan" that represents output text, tool calls, usage, and failure timing once, then render it into each OpenAI surface. That avoids the most likely long-term bug: streaming and non-streaming behavior drifting apart, or `/v1/responses` and `/v1/chat/completions` diverging because they were implemented separately.

The other key decision is to build a neutral admin control plane instead of encoding behavior in magic prompts or downstream-specific conventions. A scripted scenario queue is more explicit, easier to test, and robust enough to drive hangs, truncation, and delayed chunks without polluting the OpenAI-compatible surface.

## File Structure

Create and own the code with these boundaries:

- `Cargo.toml`: crate metadata and dependencies.
- `src/lib.rs`: public server bootstrap API for tests and the binary.
- `src/main.rs`: runtime entrypoint and environment-based config loading.
- `src/config.rs`: bind address, auth mode, and admin-route configuration.
- `src/app.rs`: router construction and shared state wiring.
- `src/state.rs`: top-level application state and synchronization primitives.
- `src/openai/mod.rs`: route registration and shared endpoint helpers.
- `src/openai/auth.rs`: bearer-token enforcement for `/v1/*`.
- `src/openai/models.rs`: serde request/response models and shared validation helpers.
- `src/openai/responses.rs`: `/v1/responses` handler and renderer adapter.
- `src/openai/chat_completions.rs`: `/v1/chat/completions` handler and renderer adapter.
- `src/engine/mod.rs`: orchestration entrypoint from HTTP requests into deterministic execution.
- `src/engine/scenario.rs`: scenario definition, matchers, and one-shot consumption rules.
- `src/engine/defaults.rs`: deterministic fallback behavior when no scenario matches.
- `src/engine/plan.rs`: canonical internal response plan and stream event plan.
- `src/engine/failures.rs`: modeled transport/application failure behaviors and timing.
- `src/admin.rs`: admin routes for scenario load/reset/request-log retrieval.
- `src/logs.rs`: request-log structures and admin serialization.
- `src/sse.rs`: SSE encoding, chunk flushing, and stream completion helpers.
- `tests/common/mod.rs`: ephemeral server harness, admin helpers, and HTTP/SSE client helpers.
- `tests/health_and_auth.rs`: healthcheck plus auth and malformed-request coverage.
- `tests/responses_contract.rs`: `/v1/responses` success and validation cases.
- `tests/chat_completions_contract.rs`: `/v1/chat/completions` success and validation cases.
- `tests/tool_and_schema_contract.rs`: tool calls, continuation, and structured output cases.
- `tests/failure_modes.rs`: status-code errors, retry headers, hangs, partial streams, truncation, and malformed SSE.
- `README.md`: generic usage, supported surface, and local run instructions.
- `docs/compatibility-matrix.md`: explicit supported request fields, unsupported fields, and failure-model capabilities.

## Contracts and Boundaries To Lock Down Before Coding

1. Canonical plan model
   - Represent one request result as a `ResponsePlan`.
   - Include output text segments, tool calls, optional structured JSON payload, usage data, delays, and terminal status.
   - Make stream rendering a pure projection of `ResponsePlan`, never a separate business path.

2. Scenario scripting model
   - A scenario should contain a matcher and a response script.
   - Matchers should support endpoint, model name, stream flag, and optional metadata tags or request-substring checks.
   - Scripts should support: success payload, OpenAI-style error response, delay before headers, delay between chunks, hang forever, close after N chunks, and malformed/truncated final body.
   - Consumption should be FIFO among matching scenarios so tests can script multi-call flows deterministically.

3. Deterministic default behavior
   - The fallback path should not require admin setup.
   - Default text behavior: extract user text inputs/messages, normalize whitespace, and return a stable synthetic summary/echo form.
   - Default tool behavior: do not infer tool calls from arbitrary prompts; only emit tool calls when a scripted scenario requests them.
   - Default structured-output behavior: if a supported `json_schema` response format is requested, return deterministic JSON matching the schema only for the supported primitive/object subset; reject unsupported schema constructs explicitly.

4. Endpoint compatibility
   - `/v1/responses` must support both stream and non-stream create flows, including continuation input items such as tool outputs when present.
   - `/v1/chat/completions` must support both stream and non-stream chat flows using the same canonical plan.
   - `/v1/responses` must accept optional `previous_response_id`, `metadata`, `stop`, reasoning requests, and image inputs without requiring downstream-specific behavior.
   - `/v1/chat/completions` must accept `tools`, `tool_choice`, `response_format`, `stop`, and assistant reasoning content in both stream and non-stream modes.
   - Request validation should reject unsupported combinations with stable OpenAI-shaped error JSON rather than silently ignoring them.

5. Transport-failure mechanics
   - Use real async streaming bodies so timing and truncation are observable over actual sockets.
   - Guard hang tests with explicit timeouts in the test harness.
   - Keep low-level transport failure code isolated in `engine/failures.rs` and `sse.rs`; handlers should describe failures, not hand-roll socket behavior.

## Task 1: Bootstrap the async Rust service skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/config.rs`
- Create: `src/app.rs`
- Create: `src/state.rs`
- Create: `src/admin.rs`
- Create: `src/openai/mod.rs`
- Create: `src/openai/auth.rs`
- Create: `tests/common/mod.rs`
- Create: `tests/health_and_auth.rs`
- Create: `README.md`

- [ ] **Step 1: Identify or write the failing test**

Write integration tests that prove the server boots, `GET /healthz` returns `200`, `/v1/*` rejects missing bearer auth, and a syntactically valid authenticated request currently fails because the endpoint handlers are not implemented yet.

```rust
#[tokio::test]
async fn healthz_is_available() { /* ... */ }

#[tokio::test]
async fn responses_requires_bearer_auth() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test healthz_is_available -- --exact`
Expected: FAIL because the crate and test harness do not exist yet.

- [ ] **Step 3: Write minimal implementation**

Create the crate, wire `tokio` + `axum`, add application state, implement `GET /healthz`, add auth middleware for `/v1/*`, expose an app-construction function from `src/lib.rs`, and add a minimal `README.md` that describes the service generically.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test healthz_is_available responses_requires_bearer_auth -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Tighten config loading and router composition, then run the targeted tests and the current full suite.

Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src tests README.md
git commit -m "feat: bootstrap async rust fake openai server"
```

## Task 2: Define request models and the canonical response-plan engine

**Files:**
- Create: `src/openai/models.rs`
- Create: `src/engine/mod.rs`
- Create: `src/engine/plan.rs`
- Create: `src/engine/defaults.rs`
- Modify: `src/openai/mod.rs`
- Modify: `src/state.rs`
- Create: `tests/responses_contract.rs`

- [ ] **Step 1: Identify or write the failing test**

Write `/v1/responses` contract tests for authenticated non-streaming requests that should return deterministic text, stable `response.id` formatting, output items, usage fields, and acceptance of the request fields the service must support generically: `metadata`, `stop`, `previous_response_id`, optional org/project headers, and image inputs.

```rust
#[tokio::test]
async fn responses_create_returns_deterministic_non_stream_payload() { /* ... */ }

#[tokio::test]
async fn responses_accepts_supported_openai_request_fields() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test responses_create_returns_deterministic_non_stream_payload -- --exact`
Expected: FAIL with `404` or unimplemented handler.

- [ ] **Step 3: Write minimal implementation**

Add serde request/response models for the supported `/v1/responses` subset, including `metadata`, `stop`, `previous_response_id`, reasoning request fields, and image inputs. Implement `ResponsePlan`, deterministic fallback extraction of user text input, stable IDs/timestamps per response, and a valid non-stream JSON payload from a real handler. Accept org/project headers as inert compatibility inputs rather than rejecting them.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test responses_create_returns_deterministic_non_stream_payload -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Move validation and fallback rendering into focused engine modules so handlers stay thin, then run targeted checks plus the broader suite.

Run: `cargo test responses_create_returns_deterministic_non_stream_payload -- --exact`
Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/openai/models.rs src/engine src/openai/mod.rs src/state.rs tests/responses_contract.rs
git commit -m "feat: add canonical response plan and responses api baseline"
```

## Task 3: Add streaming for `/v1/responses` from the canonical plan

**Files:**
- Create: `src/sse.rs`
- Modify: `src/openai/responses.rs`
- Modify: `src/engine/plan.rs`
- Modify: `src/engine/defaults.rs`
- Modify: `tests/common/mod.rs`
- Modify: `tests/responses_contract.rs`

- [ ] **Step 1: Identify or write the failing test**

Extend the `/v1/responses` tests to assert valid SSE framing, ordered events, flush behavior, and terminal completion for `stream=true`, including reasoning deltas and tool-call-related event sequences when the canonical plan contains them.

```rust
#[tokio::test]
async fn responses_stream_emits_expected_sse_sequence() { /* ... */ }

#[tokio::test]
async fn responses_stream_emits_reasoning_and_completion_events() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test responses_stream_emits_expected_sse_sequence -- --exact`
Expected: FAIL because streaming is not implemented or event order is wrong.

- [ ] **Step 3: Write minimal implementation**

Add a streaming renderer that projects `ResponsePlan` into SSE events, including terminal completion, reasoning events, and tool-call-related events required by the supported `/v1/responses` subset. Keep the non-stream and stream paths backed by the same `ResponsePlan` instance so content does not drift.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test responses_stream_emits_expected_sse_sequence -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Extract reusable SSE helpers and add assertions that the streamed text and non-stream text are semantically identical for the same request.

Run: `cargo test responses_stream_emits_expected_sse_sequence -- --exact`
Run: `cargo test responses_create_returns_deterministic_non_stream_payload -- --exact`
Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/sse.rs src/openai/responses.rs src/engine tests/common/mod.rs tests/responses_contract.rs
git commit -m "feat: stream responses api events from canonical plans"
```

## Task 4: Add scenario scripting and request logging through the admin API

**Files:**
- Create: `src/engine/scenario.rs`
- Create: `src/logs.rs`
- Modify: `src/admin.rs`
- Modify: `src/state.rs`
- Modify: `src/engine/mod.rs`
- Modify: `src/engine/defaults.rs`
- Create: `tests/tool_and_schema_contract.rs`

- [ ] **Step 1: Identify or write the failing test**

Write admin tests that preload one-shot scenarios, verify that matching OpenAI requests consume them in FIFO order, and verify that `GET /__admin/requests` returns normalized request logs.

```rust
#[tokio::test]
async fn admin_loaded_scenarios_are_consumed_fifo() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test admin_loaded_scenarios_are_consumed_fifo -- --exact`
Expected: FAIL because admin scripting and logs do not exist yet.

- [ ] **Step 3: Write minimal implementation**

Implement scenario definitions, matcher evaluation, atomic one-shot consumption, request logging, `/__admin/scenarios`, `/__admin/reset`, and `/__admin/requests`. Make matching generic: endpoint, model, stream flag, metadata tags, and text-substring checks are enough for phase one.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test admin_loaded_scenarios_are_consumed_fifo -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Ensure the admin wire format is stable and documented, then re-run the targeted checks and full suite.

Run: `cargo test admin_loaded_scenarios_are_consumed_fifo -- --exact`
Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/engine/scenario.rs src/logs.rs src/admin.rs src/state.rs src/engine tests/tool_and_schema_contract.rs
git commit -m "feat: add scenario scripting and request log admin api"
```

## Task 5: Implement tool calls, continuation, and structured outputs

**Files:**
- Modify: `src/openai/models.rs`
- Modify: `src/openai/responses.rs`
- Modify: `src/engine/plan.rs`
- Modify: `src/engine/scenario.rs`
- Modify: `src/engine/defaults.rs`
- Modify: `tests/tool_and_schema_contract.rs`
- Create: `docs/compatibility-matrix.md`

- [ ] **Step 1: Identify or write the failing test**

Add tests for:

- scripted tool-call output on `/v1/responses`
- continuation input that includes tool output items
- reasoning-bearing assistant turns and `previous_response_id` continuation acceptance
- deterministic `json_schema` structured output for the supported subset
- deterministic `json_object` output for the simpler structured-output mode
- explicit rejection of unsupported schema constructs

```rust
#[tokio::test]
async fn responses_supports_scripted_tool_call_and_continuation() { /* ... */ }

#[tokio::test]
async fn responses_structured_output_support_is_explicit() { /* ... */ }

#[tokio::test]
async fn responses_reasoning_and_continuation_fields_round_trip() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test responses_supports_scripted_tool_call_and_continuation -- --exact`
Run: `cargo test responses_structured_output_support_is_explicit -- --exact`
Expected: FAIL because tool and schema flows are incomplete.

- [ ] **Step 3: Write minimal implementation**

Extend the canonical plan model to carry tool calls, reasoning segments, and structured JSON outputs. Support scripted tool-call emissions and continuation inputs on `/v1/responses`, including `function_call_output` items and optional `previous_response_id`. Implement explicit `json_object` support plus `json_schema` support for a documented subset only, and reject everything else with stable OpenAI-shaped errors.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test responses_supports_scripted_tool_call_and_continuation -- --exact`
Run: `cargo test responses_structured_output_support_is_explicit -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Update the compatibility matrix so supported and unsupported fields are unambiguous. Then re-run all response and tool tests plus the full suite.

Run: `cargo test responses_supports_scripted_tool_call_and_continuation -- --exact`
Run: `cargo test responses_structured_output_support_is_explicit -- --exact`
Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/openai/models.rs src/openai/responses.rs src/engine docs/compatibility-matrix.md tests/tool_and_schema_contract.rs
git commit -m "feat: support scripted tool calls and structured outputs"
```

## Task 6: Add `/v1/chat/completions` on the same engine

**Files:**
- Create: `src/openai/chat_completions.rs`
- Modify: `src/openai/mod.rs`
- Modify: `src/openai/models.rs`
- Modify: `src/engine/plan.rs`
- Create: `tests/chat_completions_contract.rs`

- [ ] **Step 1: Identify or write the failing test**

Write non-stream and stream contract tests for `/v1/chat/completions` proving that the same scenario/default behavior can be rendered into chat-completion JSON and delta SSE events while accepting the supported request features for this endpoint: tools, `tool_choice`, `response_format`, `stop`, and reasoning content.

```rust
#[tokio::test]
async fn chat_completions_non_stream_uses_same_canonical_plan() { /* ... */ }

#[tokio::test]
async fn chat_completions_stream_uses_same_canonical_plan() { /* ... */ }

#[tokio::test]
async fn chat_completions_accepts_supported_openai_compatible_fields() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test chat_completions_non_stream_uses_same_canonical_plan -- --exact`
Run: `cargo test chat_completions_stream_uses_same_canonical_plan -- --exact`
Expected: FAIL because the endpoint is not implemented yet.

- [ ] **Step 3: Write minimal implementation**

Add request/response models for the supported chat-completions subset and render chat success and stream deltas from the existing canonical plan instead of adding a second behavior engine. Ensure tool calls, `tool_choice`, `response_format`, `stop`, and assistant reasoning content are accepted and rendered consistently between stream and non-stream paths.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test chat_completions_non_stream_uses_same_canonical_plan -- --exact`
Run: `cargo test chat_completions_stream_uses_same_canonical_plan -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Eliminate duplication between the endpoint adapters and verify that cross-endpoint equivalence holds where expected.

Run: `cargo test chat_completions_non_stream_uses_same_canonical_plan -- --exact`
Run: `cargo test chat_completions_stream_uses_same_canonical_plan -- --exact`
Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/openai/chat_completions.rs src/openai/mod.rs src/openai/models.rs src/engine/plan.rs tests/chat_completions_contract.rs
git commit -m "feat: add chat completions compatibility surface"
```

## Task 7: Implement application and transport failure injection

**Files:**
- Create: `src/engine/failures.rs`
- Modify: `src/engine/scenario.rs`
- Modify: `src/admin.rs`
- Modify: `src/sse.rs`
- Modify: `tests/common/mod.rs`
- Create: `tests/failure_modes.rs`
- Modify: `docs/compatibility-matrix.md`
- Modify: `README.md`

- [ ] **Step 1: Identify or write the failing test**

Add real-socket tests for:

- OpenAI-shaped JSON errors with status `400`, `401`, `403`, `404`, `408`, `413`, `429`, `500`, `502`, `503`, `504`
- quota-exceeded and content-filter error bodies that classify differently from generic invalid requests
- `Retry-After` propagation on scripted rate limits
- delayed first byte
- hang forever guarded by timeout
- partial SSE then close
- malformed/truncated SSE body

```rust
#[tokio::test]
async fn scripted_hang_times_out_client_side() { /* ... */ }

#[tokio::test]
async fn scripted_partial_stream_then_close_is_observable() { /* ... */ }

#[tokio::test]
async fn scripted_budget_and_content_filter_errors_are_distinct() { /* ... */ }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test scripted_hang_times_out_client_side -- --exact`
Run: `cargo test scripted_partial_stream_then_close_is_observable -- --exact`
Expected: FAIL because failure injection is not implemented yet.

- [ ] **Step 3: Write minimal implementation**

Implement failure scripts for status errors, quota/content-filter error variants, header delays, inter-event delays, hangs, close-after-N-chunks, and malformed/truncated stream endings. Support explicit `Retry-After` control for rate-limit scenarios. Keep these paths data-driven through scenarios rather than hard-coded test hooks.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test scripted_hang_times_out_client_side -- --exact`
Run: `cargo test scripted_partial_stream_then_close_is_observable -- --exact`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Make failure behavior documentation explicit and re-run the full suite to ensure the low-level transport code has not broken happy paths.

Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add src/engine/failures.rs src/engine/scenario.rs src/admin.rs src/sse.rs tests/common/mod.rs tests/failure_modes.rs docs/compatibility-matrix.md README.md
git commit -m "feat: add scripted application and transport failures"
```

## Task 8: Final polish, docs, and whole-project verification

**Files:**
- Modify: `README.md`
- Modify: `docs/compatibility-matrix.md`
- Modify: any touched source or tests needed for final cleanup

- [ ] **Step 1: Identify or write the failing test**

Identify any remaining gaps from the final full-suite pass. If no behavioral gap remains, treat the failing check as documentation incompleteness: make sure the README explains local startup, admin scripting, and the supported compatibility matrix clearly enough that a new engineer can run it without guesswork.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: any remaining issues are real and concrete; if all pass, proceed directly to Step 3 as a docs-and-cleanup pass.

- [ ] **Step 3: Write minimal implementation**

Resolve any final defects, tighten docs, confirm unsupported behaviors are explicitly documented, and remove dead code or duplicated helpers introduced during the build-out.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 5: Refactor and verify**

Run one final end-to-end verification pass on the complete repository.

Run: `cargo test`
Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add README.md docs/compatibility-matrix.md src tests
git commit -m "docs: finalize fake openai compatibility and usage guidance"
```

## Execution Notes

- Prefer small focused modules over large endpoint files; the canonical plan is the seam that keeps the codebase understandable.
- Do not add persistence, databases, or external queues. In-memory state is the correct steady-state for deterministic local black-box testing.
- Do not implement undocumented endpoint variants "just in case." Add only the explicitly documented supported surface and fail clearly elsewhere.
- Keep the compatibility matrix concrete. The implementation is only done when the docs and tests name the exact supported request fields and failure scripts for both `/v1/responses` and `/v1/chat/completions`.
- For malformed/truncated stream tests, use lower-level response reading when `reqwest` normalizes away the exact transport symptom.
- Keep timestamps and IDs deterministic enough for assertions without freezing the entire clock globally; injecting a clock/ID generator through state is the clean path.
- Never weaken a valid test to get green. If a transport failure test flakes, fix the timing/control mechanism rather than loosening the assertion into uselessness.
