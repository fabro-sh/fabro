# Debug Web UI Test Plan

The agreed testing strategy holds after reconciling it with the implementation plan. The plan adds two new routes (`GET /__debug` and `GET /__debug/state.json`) gated behind the existing `enable_admin` config flag, plus new snapshot types in `src/state.rs` and a `script_kind()` accessor on `ScenarioScript`. No strategy change requiring user approval was identified.

One clarification: the strategy calls for "3-4 HTTP integration tests against the debug endpoint." After reviewing the implementation plan, the highest-value integration tests are: (1) empty-state HTML page serves correctly, (2) JSON state endpoint returns correct snapshot after scenarios are loaded, (3) HTML page reflects loaded scenarios and request logs, and (4) debug routes are hidden when `enable_admin` is false. That is exactly 4 tests. The headless Chrome screenshot and HTML safety check round out the plan at 6 new tests, plus the 50-test regression gate.

## Harness requirements

1. **Ephemeral server harness** (existing: `tests/common/mod.rs`)
   - What it does: boots the real `tokio` + `axum` server on an ephemeral localhost port with `enable_admin: true` and `require_auth: true`, exposing `base_url`, authenticated client, and unauthenticated client.
   - Tests depending on it: 1 through 6.

2. **Unauthenticated HTTP client** (existing on `TestServer`)
   - What it does: the `TestServer.client` field is an unauthenticated `reqwest::Client` used to hit routes that do not require bearer auth (like `/__debug`). No new helper needed.
   - Tests depending on it: 1, 2, 3, 4.

3. **Admin scenario harness** (existing on `TestServer`)
   - What it does: scripts server state via `enqueue_scenarios()`, drives OpenAI requests via `post_responses()`, and fetches request logs via `request_logs()`.
   - Tests depending on it: 2, 3.

4. **Headless Chrome harness** (new, lightweight)
   - What it does: launches headless Chrome/Chromium via `std::process::Command` to capture a screenshot of the debug page. Falls back to skip if no Chrome binary is found.
   - What it exposes: a PNG file written to a temp path; assertion that the file is non-empty and is a valid PNG (starts with the PNG magic bytes).
   - Estimated complexity: low. No new crate; uses `chromium --headless --screenshot` CLI.
   - Tests depending on it: 5.

## Test plan

### 1. Name: debug HTML page serves valid HTML with correct content-type on empty state

- **Type**: integration
- **Disposition**: new
- **Harness**: Ephemeral server harness
- **Preconditions**: server is running with `enable_admin: true`; no scenarios loaded; no requests made.
- **Actions**:
  - `GET /__debug` using the unauthenticated client.
- **Expected outcome**:
  - HTTP status is `200`.
  - `content-type` header contains `text/html`.
  - Response body contains `<!DOCTYPE html>` (well-formed HTML document).
  - Response body contains the page title text `twin-openai` and `debug`.
  - Response body contains the empty-state marker text `no active namespaces` (since no scenarios have been loaded and no requests have been made).
  - Source of truth: implementation plan (route `GET /__debug`, HTML rendering with empty state indicator).
- **Interactions**: axum routing, `enable_admin` gate, `debug_snapshot()`, `render_html()`.

### 2. Name: debug JSON endpoint returns correct state snapshot after scenarios are loaded and requests are made

- **Type**: integration
- **Disposition**: new
- **Harness**: Ephemeral server harness, Admin scenario harness
- **Preconditions**: server is running with `enable_admin: true`.
- **Actions**:
  1. Load two scenarios via `POST /__admin/scenarios` with bearer auth: one `success` script matching `responses` endpoint with model `gpt-test`, and one `error` script matching `responses` endpoint with model `gpt-error`.
  2. Make one `POST /v1/responses` request with bearer auth, model `gpt-test`, input `"hello debug"`, `stream: false` -- this consumes the first scenario and logs a request.
  3. `GET /__debug/state.json` using the unauthenticated client.
- **Expected outcome**:
  - HTTP status is `200`.
  - `content-type` header contains `application/json`.
  - Response body parses as JSON with a top-level `namespaces` array.
  - The namespace array contains exactly one entry whose `key` field starts with `"Bearer:"`.
  - That namespace's `scenarios` array has exactly 1 remaining scenario (the `error` script; the `success` script was consumed).
  - The remaining scenario has `endpoint: "responses"`, `model: "gpt-error"`, `script_kind: "error"`.
  - That namespace's `request_logs` array has exactly 1 entry with `endpoint: "responses"`, `model: "gpt-test"`, `input_text` containing `"hello debug"`.
  - Source of truth: implementation plan (JSON API shape, `DebugSnapshot` / `NamespaceSnapshot` / `ScenarioSnapshot` structs, FIFO consumption model).
- **Interactions**: admin scenario loading, OpenAI responses endpoint, `debug_snapshot()`, JSON serialization.

### 3. Name: debug HTML page reflects loaded scenarios and request logs in rendered output

- **Type**: integration
- **Disposition**: new
- **Harness**: Ephemeral server harness, Admin scenario harness
- **Preconditions**: server is running with `enable_admin: true`.
- **Actions**:
  1. Load one `success` scenario via `POST /__admin/scenarios` with bearer auth, matching `responses` endpoint, model `gpt-html-test`.
  2. Make one `POST /v1/responses` request with bearer auth, model `gpt-other`, input `"check the page"`, `stream: false` (default behavior, does not consume the scenario because model does not match).
  3. `GET /__debug` using the unauthenticated client.
- **Expected outcome**:
  - HTTP status is `200`.
  - Response body contains the scenario's model name `gpt-html-test` in the rendered HTML (proving scenarios appear).
  - Response body contains the text `success` (the `script_kind` of the loaded scenario).
  - Response body contains the text `gpt-other` (the model from the request log).
  - Response body contains `check the page` (the input text from the request log).
  - Response body does NOT contain `no active namespaces` (because there is at least one namespace).
  - Source of truth: implementation plan (HTML page structure showing scenarios table and request log table per namespace).
- **Interactions**: admin scenario loading, OpenAI responses endpoint, `debug_snapshot()`, `render_html()`, HTML escaping.

### 4. Name: debug routes are not accessible when enable_admin is false

- **Type**: integration
- **Disposition**: new
- **Harness**: Custom server setup (not the default `spawn_server`, which uses `enable_admin: true`)
- **Preconditions**: server is started with `enable_admin: false` (construct `Config` directly and call `build_app_with_config`).
- **Actions**:
  - `GET /__debug` using an unauthenticated client.
  - `GET /__debug/state.json` using an unauthenticated client.
- **Expected outcome**:
  - Both requests return HTTP `404` (the routes are not registered when admin is disabled).
  - Source of truth: implementation plan (`debug_ui::router()` is only merged when `enable_admin` is true, same as `admin::router()`).
- **Interactions**: `app::router()` conditional routing, config flag.

### 5. Name: debug page renders visually in headless Chrome and produces a non-empty screenshot

- **Type**: scenario
- **Disposition**: new
- **Harness**: Ephemeral server harness, Headless Chrome harness
- **Preconditions**: server is running with `enable_admin: true`; headless Chrome/Chromium is available on PATH. Test is skipped if Chrome is not found.
- **Actions**:
  1. Load one scenario and make one request (to populate state for a non-trivial render).
  2. Run headless Chrome: `chromium --headless --disable-gpu --screenshot=/tmp/<unique>.png --window-size=1280,900 <base_url>/__debug` (or `google-chrome` / `chromium-browser` depending on platform).
  3. Read the output PNG file.
- **Expected outcome**:
  - The Chrome process exits with code 0.
  - The screenshot file exists and is at least 10 KB (a non-trivial rendered page, not a blank screen).
  - The first 8 bytes of the file match the PNG magic number (`\x89PNG\r\n\x1a\n`).
  - Source of truth: agreed testing strategy (headless Chrome screenshot capture, 1 test case).
- **Interactions**: full server stack, HTML rendering, CSS rendering in a real browser engine.

### 6. Name: HTML output escapes user-controlled values to prevent injection

- **Type**: unit
- **Disposition**: new
- **Harness**: Ephemeral server harness, Admin scenario harness
- **Preconditions**: server is running with `enable_admin: true`.
- **Actions**:
  1. Load a scenario via `POST /__admin/scenarios` where the model field contains an HTML injection attempt: `<script>alert('xss')</script>`.
  2. `GET /__debug` using the unauthenticated client.
- **Expected outcome**:
  - HTTP status is `200`.
  - Response body contains the escaped form `&lt;script&gt;` (proving the `escape_html` function is applied).
  - Response body does NOT contain the literal unescaped string `<script>alert` (proving no raw injection).
  - Source of truth: implementation plan (`escape_html` helper replaces `<`, `>`, `&`, `"`, `'` with entities), agreed testing strategy (HTML safety check).
- **Interactions**: admin scenario loading, `render_html()`, `escape_html()`.

### 7. Name: all 50 existing tests pass as regression gate

- **Type**: regression
- **Disposition**: existing
- **Harness**: `cargo test`
- **Preconditions**: all implementation changes are complete.
- **Actions**:
  - Run `cargo test` (the full test suite).
- **Expected outcome**:
  - All 50 existing tests pass. Zero failures, zero errors.
  - The new debug UI code does not break any existing endpoint behavior, auth handling, scenario consumption, streaming, or admin routes.
  - Source of truth: agreed testing strategy (existing test suite as regression gate).
- **Interactions**: entire codebase.

## Test file placement

All new tests should be placed in a single new file: `tests/debug_ui.rs`. This follows the existing convention where each test file in `tests/` covers a distinct feature area (e.g., `tests/health_and_auth.rs`, `tests/responses_contract.rs`, `tests/failure_modes.rs`). The file should `mod common;` to reuse the existing test server harness.

For test 4 (admin disabled), the test should construct a custom `Config` with `enable_admin: false` and use `twin_openai::build_app_with_config()` directly, similar to the pattern in `tests/config_contract.rs`.

For test 5 (headless Chrome), the test should detect available Chrome binaries at the start and skip (return early with a message) if none are found, ensuring CI environments without Chrome do not fail.
