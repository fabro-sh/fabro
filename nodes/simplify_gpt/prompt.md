Goal: # Plan: Map `reasoning_effort` to Anthropic `output_config.effort`

## Context

The Anthropic Messages API now supports `output_config: { effort: "low" | "medium" | "high" | "max" }` as the recommended way to control thinking depth for Claude Opus 4.6 and Sonnet 4.6 (replacing deprecated `budget_tokens`). Fabro's unified `Request` already carries `reasoning_effort: Option<String>` and workflow nodes default it to `"high"`, but the Anthropic provider silently drops this field — it never appears in the API request. The OpenAI provider correctly maps it to `reasoning: { effort }`, but the Anthropic provider has no equivalent.

## Single file to modify

`lib/crates/fabro-llm/src/providers/anthropic.rs`

## Step 1: Add failing tests

Add tests at the end of the `#[cfg(test)] mod tests` block (before line 2043's `}`). These tests reference a new `output_config` field on `ApiRequest` that doesn't exist yet, so they won't compile until Step 2.

**Test A** — `build_api_request_maps_reasoning_effort_to_output_config`: Build a `Request` with `reasoning_effort: Some("medium")`, call `build_api_request`, assert `api_request.output_config == Some(json!({"effort": "medium"}))`. Pattern: follows existing `build_api_request_omits_whitespace_only_system_prompt` test (line 1592).

**Test B** — `build_api_request_omits_output_config_when_no_reasoning_effort`: Same but with `reasoning_effort: None`, assert `api_request.output_config.is_none()`.

## Step 2: Add `output_config` field to `ApiRequest` struct

At line 108 (after `thinking`), add:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
output_config: Option<serde_json::Value>,
```

Update every existing `ApiRequest` construction to include `output_config: None`:
- line 1566 (serialization test)
- line 1962 (merge_provider_options test)
- line 1993 (merge_provider_options test)

At this point: code compiles, Test B passes, **Test A fails** (proving the bug exists).

## Step 3: Map `reasoning_effort` → `output_config` in `build_api_request`

At line 1073 (next to the `thinking` extraction), add:

```rust
let output_config = request
    .reasoning_effort
    .as_ref()
    .map(|effort| serde_json::json!({"effort": effort}));
```

Add `output_config,` to the `ApiRequest` construction at line 1075.

This mirrors the OpenAI provider pattern at `openai.rs:385-388`.

All tests pass.

## Verification

```sh
cargo test -p fabro-llm -- anthropic::tests::build_api_request_maps_reasoning_effort
cargo test -p fabro-llm -- anthropic::tests::build_api_request_omits_output_config
cargo test -p fabro-llm  # all provider tests still pass
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
  - Model: claude-opus-4-6, 10.6k tokens in / 4.4k out
  - Files: /home/daytona/workspace/lib/crates/fabro-llm/src/providers/anthropic.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 26.9k tokens in / 7.9k out
  - Files: /home/daytona/workspace/lib/crates/fabro-llm/src/providers/anthropic.rs


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