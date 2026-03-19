Goal: # Fix: Add OAuth callback URLs to CLI-generated GitHub App manifest

## Context

GitHub issue #97: `fabro install` creates a GitHub App that passes `fabro doctor` but cannot be used to log into `fabro-web`. The CLI manifest (`install.rs:340-354`) omits `callback_urls` and `setup_url`, so GitHub rejects the OAuth login flow with "This GitHub App must be configured with a callback URL."

The web setup flow (`setup.tsx:15-31`) already includes these fields correctly.

## Changes

### 1. Add `--web-url` flag to the `Install` command

**File:** `lib/crates/fabro-cli/src/main.rs`

Convert the `Install` variant from a unit variant (line 121) to a struct variant with a `--web-url` option:

```rust
/// Set up the Fabro environment (LLMs, certs, GitHub)
Install {
    /// Base URL for the web UI (used for OAuth callback URLs)
    #[arg(long, default_value = "http://localhost:5173")]
    web_url: String,
},
```

Update the match arm (line 833) to pass `web_url` through:

```rust
Command::Install { web_url } => {
    install::run_install(&web_url).await?;
}
```

Update the command-name match (line ~481) if it pattern-matches on `Install`.

### 2. Accept `web_url` in `run_install` and pass to `setup_github_app`

**File:** `lib/crates/fabro-cli/src/install.rs`

- Change `pub async fn run_install()` (line 553) signature to `pub async fn run_install(web_url: &str)`
- Change `async fn setup_github_app(arc_dir: &Path, s: &Styles)` (line 325) to also accept `web_url: &str`
- Pass `web_url` through at the call site inside `run_install`

### 3. Add `callback_urls` and `setup_url` to the manifest

**File:** `lib/crates/fabro-cli/src/install.rs` (lines 340-354)

Use the `web_url` parameter to construct the OAuth URLs:

```rust
let manifest = serde_json::json!({
    "name": app_name,
    "url": "https://github.com/apps/arc",
    "redirect_url": format!("http://127.0.0.1:{port}/callback"),
    "callback_urls": [format!("{web_url}/auth/callback")],
    "setup_url": format!("{web_url}/setup/callback"),
    "public": false,
    "default_permissions": {
        "contents": "write",
        "metadata": "read",
        "pull_requests": "write",
        "checks": "write",
        "issues": "write",
        "emails": "read"
    },
    "default_events": []
});
```

### 4. Add unit test for manifest callback fields

**File:** `lib/crates/fabro-cli/src/install.rs` (test module at bottom of file)

Extract a `build_github_app_manifest(port: u16, web_url: &str) -> serde_json::Value` helper from `setup_github_app` and add a test that asserts:
- `callback_urls` contains `["{web_url}/auth/callback"]`
- `setup_url` equals `"{web_url}/setup/callback"`

## Verification

1. `cargo build -p fabro-cli` — compiles without errors
2. `cargo test -p fabro-cli` — all tests pass including new manifest test
3. `cargo clippy --workspace -- -D warnings` — no lint warnings


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
  - Model: claude-opus-4-6, 20.5k tokens in / 4.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/install.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs


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