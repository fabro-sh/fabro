All verification passes. Let me summarize all the changes made:

## Summary of Changes

### 1. Foundation: `lib/crates/fabro-llm/src/types.rs`
- Added `speed: Option<String>` to `Request` struct (after `reasoning_effort`)
- Added `speed: Option<String>` to `Usage` struct (with `serde(default, skip_serializing_if)`, before `raw`)
- Added `speed: self.speed` to `Usage::Add` impl (left-hand side wins)
- Updated all test struct literals to include `speed: None`

### 2. Graph layer: `lib/crates/fabro-graphviz/src/graph/types.rs`
- Added `speed()` method to `Node` returning `Option<&str>` (no default, unlike `reasoning_effort` which defaults to "high")
- Added `assert_eq!(node.speed(), None)` to `node_defaults` test

### 3. Stylesheet: `lib/crates/fabro-workflows/src/stylesheet.rs`
- Added `"speed"` to `STYLESHEET_PROPERTIES` array
- Added `apply_speed_property` test

### 4. Agent config: `lib/crates/fabro-agent/src/config.rs`
- Added `pub speed: Option<String>` to `SessionConfig`
- Added `speed: None` to `Default` impl

### 5. Session wiring: `lib/crates/fabro-agent/src/session.rs`
- Added `speed: self.config.speed.clone()` to Request construction
- Added `set_speed()` method

### 6. Anthropic provider: `lib/crates/fabro-llm/src/providers/anthropic.rs`
- **6a.** Added `speed: Option<String>` to `ApiRequest` struct
- **6b.** Added `FAST_MODE_BETA_HEADER` constant (`"fast-mode-2026-02-01"`)
- **6c.** Extended `build_beta_header()` with `include_fast_mode_header: bool` parameter
- **6d.** In `build_api_request()`: set `speed` on ApiRequest, compute `is_fast`, pass to `build_beta_header()`
- **6e.** Added `speed: Option<String>` to `ApiUsage` struct
- **6f.** Map `api_resp.usage.speed` → `Usage.speed` in both non-streaming and streaming paths
- Added 3 new tests: `build_api_request_sets_speed`, `build_api_request_injects_fast_mode_beta_header`, `beta_header_includes_both_cache_and_fast_mode`

### 7. Usage tracking: `lib/crates/fabro-workflows/src/outcome.rs`
- Added `speed: Option<String>` to `StageUsage` (with `serde(default, skip_serializing_if)`)
- Updated `From<&StageUsage> for Usage` to include `speed`

### 8. Cost multiplier: `lib/crates/fabro-workflows/src/cost.rs`
- Applied 6x multiplier when `speed == "fast"`
- Added `compute_stage_cost_fast_mode_6x_multiplier` test

### 9. Backend wiring: `lib/crates/fabro-workflows/src/backend/api.rs`
- SessionConfig construction: added `speed: node.speed().map(String::from)`
- Prompt-mode Request construction: added `speed: node.speed().map(String::from)`
- Both StageUsage constructions: added `speed: response.usage.speed.clone()` / `total_usage.speed.clone()`

### 10. Other files updated with `speed: None` in struct literals:
- `fabro-agent/src/compaction.rs`, `fabro-agent/src/tools.rs`, `fabro-agent/src/types.rs`
- `fabro-hooks/src/executor.rs`
- `fabro-api/src/server.rs`
- `fabro-cli/src/doctor.rs`
- `fabro-llm/src/generate.rs` (also added `speed` to `GenerateParams`)
- `fabro-llm/src/client.rs`, `fabro-llm/src/providers/fabro_server.rs`, `fabro-llm/src/providers/gemini.rs`, `fabro-llm/src/providers/openai.rs`, `fabro-llm/src/providers/openai_compatible.rs`
- `fabro-workflows/src/backend/cli.rs`, `fabro-workflows/src/event.rs`, `fabro-workflows/src/preamble.rs`
- `fabro-llm/tests/integration.rs`, `fabro-workflows/tests/integration.rs`