Goal: # Unified WorktreeSandbox

## Context

Worktree management is currently split across two locations with duplicated logic:

1. **Parallel branches** (`parallel.rs`): Inline git commands in `ParallelHandler::execute()`, a thin `WorktreeSandbox` decorator, and separate local/remote code paths
2. **Top-level CLI run** (`run.rs`): A `setup_worktree()` function using synchronous git helpers, with the worktree path fed into a plain `LocalSandbox`

The goal is a single `WorktreeSandbox` type that wraps any `Arc<dyn Sandbox>`, manages the worktree lifecycle in `initialize()`/`cleanup()`, and eliminates the local/remote branching.

## Plan

### Step 1: Create `WorktreeSandbox` in fabro-sandbox

**New file:** `lib/crates/fabro-sandbox/src/worktree.rs`

Define:

```rust
pub enum WorktreeEvent {
    BranchCreated { branch: String, sha: String },
    WorktreeAdded { path: String, branch: String },
    WorktreeRemoved { path: String },
    Reset { sha: String },
}

pub type WorktreeEventCallback = Arc<dyn Fn(WorktreeEvent) + Send + Sync>;

pub struct WorktreeConfig {
    pub branch_name: String,
    pub base_sha: String,
    pub worktree_path: String,
    /// Skip branch creation and reset (for resume, where branch already exists).
    pub skip_branch_creation: bool,
}

pub struct WorktreeSandbox {
    inner: Arc<dyn Sandbox>,
    config: WorktreeConfig,
    event_callback: Option<WorktreeEventCallback>,
}
```

**Constructor + getters:** `new(inner, config)`, `set_event_callback()`, `branch_name()`, `base_sha()`, `worktree_path()`

**`initialize()`:**
1. If `!skip_branch_creation`: `git branch --force {branch_name} {base_sha}` via `inner.exec_command()`, emit `BranchCreated`
2. `git worktree remove --force {path}` (best-effort), then `git worktree add {path} {branch}`, emit `WorktreeAdded`
3. If `!skip_branch_creation`: `git reset --hard {base_sha}` in worktree dir, emit `Reset`

Does NOT call `inner.initialize()` — the inner sandbox's lifecycle is managed separately.

**`cleanup()`:** `git worktree remove --force {path}`, emit `WorktreeRemoved`. Does NOT call `inner.cleanup()`.

**`working_directory()`:** Returns `config.worktree_path`.

**`exec_command()`:** Defaults `working_dir` to `config.worktree_path` when `None`, delegates to inner.

**All other Sandbox methods:** Delegate to inner. Must be a manual `impl Sandbox` block (can't use `delegate_sandbox!` since it generates `initialize`/`cleanup`/`working_directory`/`exec_command` which we need to override).

All interpolated values in git commands use `shell_quote()`.

### Step 2: Register module and re-exports

- `lib/crates/fabro-sandbox/src/lib.rs`: Add `pub mod worktree;` and `pub use worktree::WorktreeSandbox;`
- `lib/crates/fabro-agent/src/sandbox.rs`: Add re-export of `WorktreeSandbox`

### Step 3: Unit tests for WorktreeSandbox

In `worktree.rs` `#[cfg(test)]` module, using `MockSandbox`:

- `initialize()` issues correct git commands (branch, worktree remove, worktree add, reset) and emits events
- `skip_branch_creation` skips branch + reset, only does worktree add
- `cleanup()` issues `worktree remove` and emits `WorktreeRemoved`
- `working_directory()` returns worktree path
- `exec_command()` with `None` working_dir defaults to worktree path
- `exec_command()` with explicit working_dir passes it through
- `initialize()` propagates errors on non-zero exit

**MockSandbox enhancement:** Add `captured_commands: Mutex<Vec<String>>` field to `test_support.rs` to capture the sequence of `exec_command` calls (current `captured_command` only stores the last one). Append to vec in `exec_command()` impl.

### Step 4: Refactor parallel.rs

- **Remove** the private `WorktreeSandbox` struct (lines 28-126) and `use fabro_agent::LocalSandbox`
- **Replace** the inline git setup loop (lines 361-450) with:
  - Construct `WorktreeConfig` with branch name, base SHA, worktree path
  - Create `WorktreeSandbox::new(Arc::clone(&services.sandbox), config)`
  - Wire event callback to bridge `WorktreeEvent` → `WorkflowRunEvent`
  - Call `initialize().await`
- This eliminates the `if services.sandbox.is_remote()` branch (lines 442-449) — `WorktreeSandbox` works the same for any inner sandbox
- **Cleanup loop** (lines 659-668): Keep calling `git_remove_worktree()` on the parent sandbox (the `WorktreeSandbox` Arc is consumed by the spawned task and dropped). Alternatively, could store the sandbox Arc in `BranchResult` and call `.cleanup()`, but the current approach is simpler.

### Step 5: Refactor run.rs — new runs

Replace `setup_worktree()` call (lines 830-845) + separate `LocalSandbox` construction with:

```
if workdir_strategy == LocalWorktree:
    base_sha = git::head_sha()
    branch_name = "fabro/run/{run_id}"
    inner = Arc::new(LocalSandbox::new(original_cwd))
    wt_sandbox = WorktreeSandbox::new(inner, WorktreeConfig { ... })
    wt_sandbox.set_event_callback(bridge to WorkflowRunEvent)
    wt_sandbox.initialize().await
    std::env::set_current_dir(&worktree_path)  // stays in CLI, not in sandbox
    sandbox = Arc::new(wt_sandbox)
    // store base_sha, branch_name for RunConfig
```

**Delete** the `setup_worktree()` function (lines 1696-1714) — its logic is absorbed above.

`std::env::set_current_dir()` stays in `run.rs` — it's a process-global side effect that belongs to the CLI.

### Step 6: Refactor run.rs — resume (run_from_branch)

Replace worktree re-attachment (lines 1810-1822) with:

```
inner = Arc::new(LocalSandbox::new(original_cwd))
wt_sandbox = WorktreeSandbox::new(inner, WorktreeConfig {
    branch_name: run_branch,
    base_sha: base_sha.unwrap_or_default(),
    worktree_path: wt_str,
    skip_branch_creation: true,  // branch already exists
})
wt_sandbox.initialize().await
std::env::set_current_dir(&wt)
```

### Step 7: Verify

- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- Manual: `fabro run` with worktree mode enabled on a local workflow
- Manual: `fabro run --run-branch` to test resume path

## Files to modify

| File | Change |
|---|---|
| `lib/crates/fabro-sandbox/src/worktree.rs` | **New** — WorktreeSandbox, WorktreeConfig, WorktreeEvent, impl Sandbox, tests |
| `lib/crates/fabro-sandbox/src/lib.rs` | Add module + re-export |
| `lib/crates/fabro-sandbox/src/test_support.rs` | Add `captured_commands: Mutex<Vec<String>>` to MockSandbox |
| `lib/crates/fabro-agent/src/sandbox.rs` | Add WorktreeSandbox re-export |
| `lib/crates/fabro-workflows/src/handler/parallel.rs` | Remove old WorktreeSandbox, use new one |
| `lib/crates/fabro-cli/src/commands/run.rs` | Replace setup_worktree + run_from_branch worktree logic |

## Functions that become removable

| Function | Location | Reason |
|---|---|---|
| `setup_worktree()` | `run.rs:1696` | Logic absorbed into WorktreeSandbox |
| Old `WorktreeSandbox` struct | `parallel.rs:28-126` | Replaced by shared WorktreeSandbox |

Engine git helpers (`git_add_worktree`, `git_remove_worktree`, etc. in `engine.rs`) stay — still used by parallel cleanup and potentially other callers. Sync git helpers in `git.rs` also stay.


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
  - Model: claude-sonnet-4-6, 190.9k tokens in / 105.0k out
  - Files: /home/daytona/workspace/lib/crates/fabro-agent/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-agent/src/sandbox.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/lib.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/test_support.rs, /home/daytona/workspace/lib/crates/fabro-sandbox/src/worktree.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/handler/parallel.rs
- **simplify_opus**: success
  - Model: claude-sonnet-4-6, 84.8k tokens in / 30.2k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/run.rs, /home/daytona/workspace/lib/crates/fabro-workflows/src/handler/parallel.rs


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