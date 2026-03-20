Goal: # Plan: Support Anthropic fast mode (`speed: fast`) — Issue #120

## Context

Anthropic's API supports [fast mode](https://platform.claude.com/docs/en/build-with-claude/fast-mode) (beta) for Claude Opus 4.6 — up to 2.5x faster output at 6x pricing ($30/$150 per MTok vs $5/$25 standard). The API requires:
- `"speed": "fast"` top-level field in request body
- `anthropic-beta: fast-mode-2026-02-01` header
- Response includes `usage.speed` confirming actual speed used

This follows the same pattern as `reasoning_effort`, which is already implemented end-to-end.

## Implementation

### 1. Foundation: `lib/crates/fabro-llm/src/types.rs`

- Add `speed: Option<String>` to `Request` (after `reasoning_effort`, line ~454)
- Add `speed: Option<String>` to `Usage` (before `raw`, line ~372)
- In `Usage::Add` impl (line ~389): propagate `speed` from `self` (left-hand side wins, since all requests in a session use the same speed)

### 2. Graph layer: `lib/crates/fabro-graphviz/src/graph/types.rs`

Add after `reasoning_effort()` (~line 211):
```rust
#[must_use]
pub fn speed(&self) -> Option<&str> {
    self.str_attr("speed")
}
```
Returns `Option` (no default) — unlike `reasoning_effort` which defaults to "high", speed defaults to None (standard) since fast mode is 6x cost.

### 3. Stylesheet: `lib/crates/fabro-workflows/src/stylesheet.rs`

Add `"speed"` to `STYLESHEET_PROPERTIES` array (line 5):
```rust
const STYLESHEET_PROPERTIES: &[&str] = &["model", "provider", "reasoning_effort", "speed", "backend"];
```

### 4. Agent config: `lib/crates/fabro-agent/src/config.rs`

- Add `pub speed: Option<String>` field to `SessionConfig` (line ~66)
- Add `speed: None` to `Default` impl (line ~135)

### 5. Session wiring: `lib/crates/fabro-agent/src/session.rs`

- Add `speed: self.config.speed.clone()` to Request construction (~line 908)
- Add `set_speed()` method analogous to `set_reasoning_effort()` (~line 458)

### 6. Anthropic provider: `lib/crates/fabro-llm/src/providers/anthropic.rs`

**6a. `ApiRequest` struct** (~line 87): Add `speed: Option<String>` field

**6b. Beta header constant** (~line 510):
```rust
const FAST_MODE_BETA_HEADER: &str = "fast-mode-2026-02-01";
```

**6c. `build_beta_header()`** (~line 582): Add `include_fast_mode_header: bool` parameter. Inject fast-mode header (same pattern as cache header).

**6d. `build_api_request()`** (~line 1082):
- Set `speed: request.speed.clone()` on `ApiRequest`
- Compute `let is_fast = request.speed.as_deref() == Some("fast")`
- Pass `is_fast` to `build_beta_header()`

**6e. `ApiUsage`** (~line 160): Add `speed: Option<String>` field

**6f. Response parsing**: Map `api_resp.usage.speed` → `Usage.speed` in both:
- Non-streaming path (~line 1192)
- Streaming `message_start` handler (~line 690)

### 7. Usage tracking: `lib/crates/fabro-workflows/src/outcome.rs`

- Add `speed: Option<String>` to `StageUsage` (with `serde(default, skip_serializing_if)`)
- Update `From<&StageUsage> for Usage` to include `speed`

### 8. Cost multiplier: `lib/crates/fabro-workflows/src/cost.rs`

Apply 6x multiplier when `speed == "fast"`:
```rust
let multiplier = if usage.speed.as_deref() == Some("fast") { 6.0 } else { 1.0 };
// Apply multiplier to final cost
```

### 9. Backend wiring: `lib/crates/fabro-workflows/src/backend/api.rs`

- SessionConfig construction (~line 195): add `speed: node.speed().map(String::from)`
- Prompt-mode Request construction (~line 285): add `speed: node.speed().map(String::from)`
- Both StageUsage constructions (~lines 389, 589): add `speed: response.usage.speed.clone()` / `total_usage.speed.clone()`

### 10. Tests

- `anthropic.rs`: `build_api_request_sets_speed`, `build_api_request_injects_fast_mode_beta_header`, `beta_header_includes_both_cache_and_fast_mode`
- `cost.rs`: `compute_stage_cost_fast_mode_6x_multiplier`
- `stylesheet.rs`: `apply_speed_property`
- `session.rs`: `set_speed_mid_session`
- Update all existing struct literals (`Request`, `Usage`, `SessionConfig`, `StageUsage`, `ApiRequest`, `ApiUsage`) in tests to include `speed: None`

## Implementation order

1. `types.rs` (Request + Usage) — everything depends on this
2. `graph/types.rs` + `stylesheet.rs` — graph layer
3. `config.rs` + `session.rs` — agent layer
4. `anthropic.rs` — provider (ApiRequest, beta header, response parsing)
5. `outcome.rs` + `cost.rs` — usage tracking
6. `backend/api.rs` — final wiring
7. Tests throughout

## Verification

```
cargo build --workspace
cargo test --workspace
cargo fmt --check --all
cargo clippy --workspace -- -D warnings
```


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: success
  - Model: claude-opus-4-6, 169.9k tokens in / 37.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-agent/src/compaction.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/config.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/session.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/tools.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/types.rs, /home/daytona/workspace/lib/crates/fabro-api/src/server.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/doctor.rs, /home/daytona/workspace/lib/crates/fabro-graphviz/src/graph/types.rs, /home/daytona/workspace/lib/crates/fabro-hooks/src/executor.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/client.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/generate.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/providers/anthropic.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/providers/fabro_server.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/providers/gemini.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/providers/openai.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/providers/openai_compatible.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/types.rs, /home/daytona/workspace/lib/crates/fabro-llm/tests/integration.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/backend/api.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/backend/cli.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/cost.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/event.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/outcome.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/preamble.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/stylesheet.rs, /home/daytona/workspace/lib/crates/fabro-workflows/tests/integration.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 38.3k tokens in / 8.3k out


# Simplify: Code Review and Cleanup

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run git diff (or git diff HEAD if there are staged changes) to see what changed. If there are no git changes, review the most recently modified files that the user mentioned or that you edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message. Pass each agent the full diff so it has the complete context.

### Agent 1: Code Reuse Review

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use Grep to find similar patterns elsewhere in the codebase — common locations are utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality. Suggest the existing function to use instead.
3. Flag any inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar patterns are common candidates.

Note: This is a greenfield app, so focus on maximizing simplicity and don't worry about changing things to achieve it.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. Parameter sprawl: adding new parameters to a function instead of generalizing or restructuring existing ones
3. Copy-paste with slight variation: near-duplicate code blocks that should be unified with a shared abstraction
4. Leaky abstractions: exposing internal details that should be encapsulated, or breaking existing abstraction boundaries
5. Stringly-typed code: using raw strings where constants, enums (string unions), or branded types already exist in the codebase

Note: This is a greenfield app, so be aggressive in optimizing quality.

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns
2. Missed concurrency: independent operations run sequentially when they could run in parallel
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths
4. Unnecessary existence checks: pre-checking file/resource existence before operating (TOCTOU anti-pattern) — operate directly and handle the error
5. Memory: unbounded data structures, missing cleanup, event listener leaks
6. Overly broad operations: reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate their findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue with the finding, just skip it.

When done, briefly summarize what was fixed (or confirm the code was already clean).