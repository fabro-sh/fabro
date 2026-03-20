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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.