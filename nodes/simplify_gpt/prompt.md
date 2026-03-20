Goal: # Plan: Merge fabro-linear and GitHub tracker into fabro-tracker

## Context

The tracker ecosystem currently has three crates:
- **fabro-tracker** — trait-only (57 lines): `Tracker` trait, `Issue`, `BlockerRef`
- **fabro-linear** — standalone Linear functions that duplicate the `Issue`/`BlockerRef` types and don't implement the `Tracker` trait. Entirely unused.
- **fabro-github** — mixed crate: GitHub App auth/PRs/branches (~70%) + `GitHubTracker` implementing `Tracker` (~30%). The `GitHubTracker` is also unused by consumers.

Goal: consolidate both tracker implementations into `fabro-tracker`, keeping non-tracker GitHub code in `fabro-github`, and deleting `fabro-linear`.

## Dependency Direction Change

**Current:** `fabro-github → fabro-tracker` (for trait re-export)
**After:** `fabro-tracker → fabro-github` (for auth primitives: `GitHubAppCredentials`, `sign_app_jwt`, `create_installation_access_token_for_projects`)

`fabro-github` no longer depends on `fabro-tracker`. No circular dependency.

## Steps

### 1. Update `lib/crates/fabro-tracker/Cargo.toml`

Add dependencies needed by both implementations:
```toml
[dependencies]
fabro-github = { path = "../fabro-github" }
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
reqwest.workspace = true
tracing.workspace = true
tokio = { workspace = true }

[dev-dependencies]
mockito = "1"
tokio = { workspace = true, features = ["test-util", "macros"] }
```

### 2. Update `lib/crates/fabro-github/Cargo.toml`

Remove:
- `fabro-tracker = { path = "../fabro-tracker" }`
- `async-trait.workspace = true`

### 3. Remove tracker code from `lib/crates/fabro-github/src/lib.rs`

Remove these items (keep everything else):
- `use async_trait::async_trait;` (line 1)
- `use tokio::sync::OnceCell;` (line 3)
- `pub use fabro_tracker::{BlockerRef, Issue, Tracker};` (line 5)
- `execute_github_graphql()` fn (~line 796)
- `GitHubTracker` struct, `impl GitHubTracker`, `impl Tracker for GitHubTracker` (~line 866-1315)
- `normalize_github_item()` fn (~line 981)
- `fetch_project_items_page()` fn (~line 1035)
- All tracker-related tests (~line 2172-2841): `execute_github_graphql` tests, tracker helpers (`mock_github_tracker`, `make_test_issue`, etc.), and all `GitHubTracker` method tests

**Keep** `create_installation_access_token_for_projects()` — it stays in fabro-github as a public function alongside the other `create_installation_access_token_*` variants.

### 4. Create `lib/crates/fabro-tracker/src/linear.rs`

Adapt `fabro-linear/src/lib.rs` code:
- **Remove** the duplicate `Issue` and `BlockerRef` struct definitions — use `crate::Issue` and `crate::BlockerRef`
- **Adapt** `normalize_issue()` to return `crate::Issue` with `project_item_id: None`
- **Create** `LinearTracker` struct wrapping `LinearConfig`, `reqwest::Client`, and `project_slug: String`
- **Implement** `Tracker for LinearTracker` by delegating to the existing functions
- **Keep** private: `execute_graphql`, `normalize_issue`, `extract_issues`, `ISSUE_FIELDS`, `BLOCKS_RELATION_TYPE`
- **Keep** public: `LinearConfig`, `LinearTracker`, `LINEAR_API_ENDPOINT`
- **Move** all tests, updating assertions for the added `project_item_id` field

### 5. Create `lib/crates/fabro-tracker/src/github.rs`

Move tracker code from fabro-github:
- Import from fabro-github: `GitHubAppCredentials`, `sign_app_jwt`, `create_installation_access_token_for_projects`
- Contains: `GitHubTracker` struct + impls, `execute_github_graphql`, `normalize_github_item`, `fetch_project_items_page`
- Uses `crate::Issue` and `crate::BlockerRef` (same types, just different import path)
- Move all GitHubTracker tests (duplicate `test_rsa_key()` helper since test code isn't importable across crates)

### 6. Update `lib/crates/fabro-tracker/src/lib.rs`

Add module declarations and re-exports:
```rust
pub mod github;
pub mod linear;

pub use github::GitHubTracker;
pub use linear::{LinearConfig, LinearTracker, LINEAR_API_ENDPOINT};
```

### 7. Delete `lib/crates/fabro-linear/`

Remove entire directory. Workspace glob `members = ["lib/crates/*"]` handles the rest.

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

Key things to verify:
- No circular dependency (fabro-tracker → fabro-github, fabro-github has no fabro-tracker dep)
- Linear `normalize_issue` sets `project_item_id: None`
- All moved tests pass in their new home
- `test_rsa_key()` helper is duplicated in fabro-tracker's github test module


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
  - Model: claude-opus-4-6, 113.7k tokens in / 38.3k out
  - Files: /home/daytona/workspace/lib/crates/fabro-github/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-github/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-tracker/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-tracker/src/github.rs, /home/daytona/workspace/lib/crates/fabro-tracker/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-tracker/src/linear.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 96.2k tokens in / 12.1k out
  - Files: /home/daytona/workspace/lib/crates/fabro-tracker/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-tracker/src/github.rs, /home/daytona/workspace/lib/crates/fabro-tracker/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-tracker/src/linear.rs


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