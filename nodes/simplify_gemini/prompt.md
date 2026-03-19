Goal: # Fix: Default provider should respect configured API keys

## Context

Users who only have OpenAI (or Gemini) configured hit an error when running workflows without an explicit `--provider` flag. The system hardcodes `Provider::Anthropic` as the fallback in several places, so it tries to call Anthropic even when no `ANTHROPIC_API_KEY` exists.

The fix: add a `Provider::default_from_env()` method that checks which providers have API keys and picks the best one, then replace all hardcoded `Provider::Anthropic` fallbacks with it.

## Approach: Red/Green TDD

Each step writes failing tests first, then implements to make them pass.

---

### Cycle 1: `Provider::default_with()` core logic

**RED** — Add tests to `lib/crates/fabro-llm/src/provider.rs` (`mod tests`):

```rust
#[test]
fn default_with_all_configured_prefers_anthropic() {
    assert_eq!(Provider::default_with(|_| true), Provider::Anthropic);
}

#[test]
fn default_with_only_openai() {
    assert_eq!(Provider::default_with(|p| p == Provider::OpenAi), Provider::OpenAi);
}

#[test]
fn default_with_only_gemini() {
    assert_eq!(Provider::default_with(|p| p == Provider::Gemini), Provider::Gemini);
}

#[test]
fn default_with_openai_and_gemini_prefers_openai() {
    assert_eq!(
        Provider::default_with(|p| p == Provider::OpenAi || p == Provider::Gemini),
        Provider::OpenAi,
    );
}

#[test]
fn default_with_none_configured_falls_back_to_anthropic() {
    assert_eq!(Provider::default_with(|_| false), Provider::Anthropic);
}

#[test]
fn default_with_only_kimi_falls_back_to_anthropic() {
    assert_eq!(Provider::default_with(|p| p == Provider::Kimi), Provider::Anthropic);
}
```

Run `cargo test -p fabro-llm` → compile error (method doesn't exist).

**GREEN** — Add to `impl Provider` in the same file:

```rust
#[must_use]
pub fn default_from_env() -> Self {
    Self::default_with(Self::has_api_key)
}

fn default_with(is_configured: impl Fn(Self) -> bool) -> Self {
    const PRECEDENCE: [Provider; 3] = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini];
    PRECEDENCE.iter().copied().find(|&p| is_configured(p)).unwrap_or(Provider::Anthropic)
}
```

Run `cargo test -p fabro-llm` → all 6 new tests pass.

---

### Cycle 2: Replace hardcoded fallbacks

These are mechanical substitutions. For each site, the change is the same pattern: `.unwrap_or(Provider::Anthropic)` → `.unwrap_or_else(Provider::default_from_env)`.

**Sites to update:**

| # | File | Line | What changes |
|---|------|------|-------------|
| 1 | `lib/crates/fabro-cli/src/commands/run.rs` | 211 | `resolve_model_provider()` provider fallback |
| 2 | `lib/crates/fabro-cli/src/commands/run.rs` | 1123 | `run_command()` provider parse fallback |
| 3 | `lib/crates/fabro-cli/src/commands/run.rs` | 1846-1853 | `run_from_branch()` — hardcoded `"claude-opus-4-6"` model + `Provider::Anthropic` |
| 4 | `lib/crates/fabro-api/src/serve.rs` | 290-310 | `resolve_model_provider()` — `catalog::default_model()` + provider fallback |
| 5 | `lib/crates/fabro-workflows/src/handler/prompt.rs` | 70 | prompt handler provider fallback |
| 6 | `lib/crates/fabro-cli/src/commands/pr.rs` | 388 | `catalog::default_model()` → provider-aware default |

**Special cases (not just unwrap_or swaps):**

- **run.rs:1846** — replace `"claude-opus-4-6".to_string()` with catalog lookup using `default_from_env()`:
  ```rust
  let default_provider = Provider::default_from_env();
  let model = args.model.unwrap_or_else(|| {
      fabro_llm::catalog::default_model_for_provider(default_provider.as_str())
          .map(|m| m.id)
          .unwrap_or_else(|| default_provider.as_str().to_string())
  });
  ```

- **serve.rs:294** — replace `catalog::default_model()` with provider-aware lookup:
  ```rust
  let default_provider = Provider::default_from_env();
  let default_info = provider_str
      .and_then(fabro_llm::catalog::default_model_for_provider)
      .unwrap_or_else(|| {
          fabro_llm::catalog::default_model_for_provider(default_provider.as_str())
              .unwrap_or_else(fabro_llm::catalog::default_model)
      });
  ```

- **pr.rs:388** — replace `catalog::default_model()` with same pattern.

Run `cargo test --workspace` after each file. Run `cargo clippy --workspace -- -D warnings` at the end.

---

## Verification

1. `cargo fmt --check --all`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo test --workspace`


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
  - Model: claude-opus-4-6, 22.8k tokens in / 5.9k out
  - Files: /home/daytona/workspace/lib/crates/fabro-api/src/serve.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/pr.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/provider.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/handler/prompt.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 48.4k tokens in / 14.9k out
  - Files: /home/daytona/workspace/lib/crates/fabro-agent/src/cli.rs, /home/daytona/workspace/lib/crates/fabro-api/src/serve.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/pr.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-llm/src/catalog.rs


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