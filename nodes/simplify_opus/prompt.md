Goal: # Plan: `fabro wait` subcommand

## Context

`fabro run` launches workflows but there's no way to block until a run completes and get its exit code — analogous to `docker wait`. This is useful for scripting (e.g., `fabro run smoke && echo "passed"`). The closest existing command is `fabro logs --follow`, which streams events and exits when `conclusion.json` appears.

## Implementation

### 1. Create `lib/crates/fabro-cli/src/commands/wait.rs`

**Args struct:**
```rust
#[derive(Args)]
pub struct WaitArgs {
    /// Run ID prefix or workflow name (most recent run)
    pub run: String,

    /// Maximum time to wait in seconds
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Poll interval in milliseconds
    #[arg(long, value_name = "MS", default_value = "1000")]
    pub interval: u64,

    /// Output conclusion as JSON
    #[arg(long)]
    pub json: bool,
}
```

**`run()` function logic:**
1. Resolve run via `fabro_workflows::run_lookup::resolve_run()` (same as `logs.rs:30`)
2. Poll `status.json` via `RunStatusRecord::load()` every `--interval` ms
3. When `status.is_terminal()`, read `conclusion.json` for summary data
4. Print human-readable status line to stderr (or `--json` to stdout)
5. Exit 0 for `Succeeded`, exit 1 for `Failed`/`Dead` (use `std::process::exit(1)` to avoid printing an error prefix, matching the pattern in `run.rs`)

**Completion detection:** Poll `status.json` (not `conclusion.json` existence) since `RunStatusRecord::load()` gives the exact status. Fall back to `Dead` if the file is missing (orphaned run).

**Timeout:** Check deadline after each sleep iteration; `bail!()` with a message if exceeded.

### 2. Register in `lib/crates/fabro-cli/src/commands/mod.rs`

Add `pub mod wait;` between `validate` and `workflow` (line 19).

### 3. Register in `lib/crates/fabro-cli/src/main.rs`

Three insertions:

**(a)** Command enum variant (after `Rewind` at ~line 149):
```rust
/// Block until a workflow run completes
Wait(commands::wait::WaitArgs),
```

**(b)** Command name mapping (~line 497):
```rust
Command::Wait(_) => "wait",
```

**(c)** Execution dispatch (~line 884, after `Rewind`):
```rust
Command::Wait(args) => {
    let styles = fabro_util::terminal::Styles::detect_stderr();
    commands::wait::run(args, &styles)?;
}
```

### Reused utilities

| Utility | Location |
|---|---|
| `resolve_run()` | `fabro_workflows::run_lookup` (run ID/name resolution) |
| `RunStatusRecord::load()` | `fabro_workflows::run_status` (poll status.json) |
| `RunStatus::is_terminal()` | `fabro_workflows::run_status` (check completion) |
| `Conclusion::load()` | `fabro_workflows::conclusion` (read duration/cost) |
| `format_duration_ms()` | `commands::shared` (human-readable duration) |
| `Styles` | `fabro_util::terminal` (colored output) |

No new dependencies needed — all are already in `fabro-cli/Cargo.toml`.

## Verification

1. `cargo build -p fabro-cli` — compiles
2. `cargo test -p fabro-cli` — existing tests pass
3. `fabro wait --help` — shows usage
4. `fabro wait <completed-run-id>` — prints status immediately, exits 0 or 1
5. Launch a run, then `fabro wait <run-id>` — blocks until completion
6. `fabro wait --timeout 1 <active-run>` — times out with error
7. `fabro wait --json <run-id>` — prints JSON to stdout


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
  - Model: claude-opus-4-6, 64.1k tokens in / 6.7k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/mod.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/commands/wait.rs, /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs


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