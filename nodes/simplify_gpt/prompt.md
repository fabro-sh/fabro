Goal: # Fix: DaytonaSandbox concurrent test failures from `set_current_dir` poisoning

## Context

Two Daytona integration tests (`daytona_computer_use_browser_screenshot` and `daytona_playwright_mcp_sandbox_transport`) call `std::env::set_current_dir(tmp.path())` to make `detect_repo_info()` fail so the sandbox skips cloning. Since `set_current_dir` is **process-global**, any concurrent test calling `initialize()` sees the changed cwd, causing `detect_repo_info` to fail and the sandbox to get an empty directory with no git repo. This makes `git rev-parse HEAD` return exit code 128.

The fix follows the existing `ExeSandbox`/`SshSandbox` pattern: move clone params out of `initialize()` and into the constructor so callers control whether cloning happens.

## Changes

### 1. `lib/crates/fabro-daytona/src/lib.rs` — Core refactor

- Add a `GitCloneParams` struct with `url: String` and `branch: Option<String>` fields
- Change `DaytonaSandbox` field from `clone_branch: Option<String>` to `clone_params: Option<GitCloneParams>`
- Update `new()` signature: last param changes from `clone_branch: Option<String>` to `clone_params: Option<GitCloneParams>`
- Update `reconnect()` (line 84): `clone_params: None`
- Refactor `initialize()`:
  - Remove `let cwd = std::env::current_dir()` (line 392)
  - Replace `match detect_repo_info(&cwd)` (line 448) with `if let Some(ref params) = self.clone_params`
  - `Some` arm: use `params.url` / `params.branch` directly (already HTTPS, no `ssh_url_to_https` needed inside initialize)
  - `None` arm: create empty working directory (existing `Err` arm logic, lines 607-618)
  - Remove `self.clone_branch.clone().or(detected_branch)` merge — caller provides the final branch

### 2. `lib/crates/fabro-cli/src/commands/run.rs` — Production callers

- **Line 1017** (main `run` path): Construct `GitCloneParams` from `origin_url` and `detected_base_branch` (already extracted at line 557):
  ```rust
  let clone_params = origin_url.as_ref().map(|url| fabro_daytona::GitCloneParams {
      url: fabro_github::ssh_url_to_https(url),
      branch: detected_base_branch.clone(),
  });
  ```
  Pass `clone_params` as the last arg to `DaytonaSandbox::new()`

- **Line 2133** (doctor path): Currently passes `None` for `clone_branch`. Under the new API, `None` for `clone_params` means "skip clone" — same behavior, just update the type. No logic change needed.

### 3. `lib/crates/fabro-workflows/tests/daytona_integration.rs` — Test fixes

- **`create_env_with_github_app`** (line 30): Detect repo and build `GitCloneParams` before calling `new()`:
  ```rust
  let cwd = std::env::current_dir().unwrap();
  let clone_params = fabro_daytona::detect_repo_info(&cwd)
      .ok()
      .map(|(url, branch)| fabro_daytona::GitCloneParams {
          url: fabro_github::ssh_url_to_https(&url),
          branch,
      });
  DaytonaSandbox::new(DaytonaConfig::default(), github_app, None, clone_params)
  ```
  This preserves cloning for all tests that use `create_env()`/`create_env_with_github_app()`.

- **`daytona_snapshot_sandbox`** (line 252) and **`run_daytona_cli_test`** (line 927): Currently pass `None` as `clone_branch`. Under new API, `None` for `clone_params` = skip clone. These tests don't need repo contents (snapshot checks `rg --version`, CLI tests install tools independently). No logic change needed.

- **`daytona_computer_use_browser_screenshot`** (line 1855-1857): Remove `tempfile::tempdir()` and `set_current_dir()`. Already passes `None` as last arg → skip clone.

- **`daytona_playwright_mcp_sandbox_transport`** (line 2015-2017): Same — remove `tempfile::tempdir()` and `set_current_dir()`.

## Verification

1. `cargo build --workspace` — confirms all callers updated (compiler catches type mismatch)
2. `cargo test -p fabro-workflows --test daytona_integration -- --ignored --test-threads=4` — the previously-failing git tests should pass with concurrent execution
3. Specifically verify the 5 previously-failing tests pass: `daytona_full_lifecycle`, `daytona_git_checkpoint_remote_emits_events`, `daytona_git_checkpoint_with_shadow_branch`, `daytona_git_push_run_branch_to_origin`, `daytona_parallel_git_branching_e2e`


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
  - Model: claude-opus-4-6, 66.4k tokens in / 10.7k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-daytona/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-workflows/tests/daytona_integration.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 33.2k tokens in / 8.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-daytona/src/lib.rs
- **simplify_gemini**: success
  - Model: claude-opus-4-6, 28.2k tokens in / 12.4k out
  - Files: /home/daytona/workspace/lib/crates/fabro-daytona/src/lib.rs


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