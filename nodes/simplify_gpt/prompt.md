Goal: # Strip markdown headings and "Plan:" prefix from GOAL column in `fabro ps`

## Context
The `GOAL` column in `fabro ps` output sometimes shows raw markdown heading markers (`#`, `##`, etc.) and a `Plan:` prefix, which is noisy. We want to strip these for cleaner output.

## Change
**File:** `lib/crates/fabro-cli/src/commands/runs.rs` — `truncate_goal` function (line 229)

Before truncating, add two stripping steps:
1. Strip leading `#` characters and any whitespace after them (e.g. `## Fix bug` → `Fix bug`)
2. Strip a leading `Plan:` prefix (case-sensitive)
3. Trim whitespace after both strips

Current code:
```rust
fn truncate_goal(goal: &str, max_len: usize) -> String {
    let line = goal.lines().next().unwrap_or("");
    // ... truncation logic
}
```

Updated logic (inserted after extracting `line`, before truncation):
```rust
let line = line.trim_start_matches('#').trim();
let line = line.strip_prefix("Plan:").map(|s| s.trim()).unwrap_or(line);
```

## Verification
- `cargo test -p fabro-cli` — run existing tests
- `cargo clippy --workspace -- -D warnings` — lint check
- Manual: `fabro ps -a` to confirm goals render without `#` or `Plan:` prefixes


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
  - Model: claude-opus-4-6, 15.2k tokens in / 2.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/runs.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 18.2k tokens in / 6.4k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/runs.rs


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