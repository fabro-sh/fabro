Goal: # Add Sentry panic reporting to fabro CLI

## Context

Fabro has no panic reporting. When the CLI panics, we lose visibility. We want Sentry-based panic reporting modeled on qlty's approach: serialize a Sentry event to a temp file, double-fork a detached subprocess to upload it.

The existing `__send_analytics` sender uses a simple `Command::spawn()` which is unreliable — the child can get killed when the parent exits or the terminal session ends. We'll fix both senders to use the double-fork pattern (fork → setsid → close_fd → fork → exec) from qlty, which fully detaches the subprocess.

## Files to modify

### 1. `Cargo.toml` (workspace root) — add deps
```toml
sentry = { version = "0.35", default-features = false, features = ["backtrace", "contexts", "ureq", "rustls"] }
fork = "0.2"
exec = "0.3"
```

### 2. `lib/crates/fabro-util/Cargo.toml` — add deps
```toml
sentry.workspace = true
fork.workspace = true
exec.workspace = true
```

### 3. `lib/crates/fabro-util/src/telemetry/mod.rs`
- Add `pub mod panic;`
- Add `pub mod spawn;`

### 4. `lib/crates/fabro-util/src/telemetry/spawn.rs` — NEW FILE

Extract a shared `spawn_detached(args: &[&str], env: &[(&str, &str)])` function used by both analytics sender and panic sender. Implements:
- **Unix**: double-fork via `fork` crate (fork → setsid → close_fd → fork → exec)
- **Windows**: `Command::new().creation_flags(DETACHED_PROCESS).spawn()`

### 5. `lib/crates/fabro-util/src/telemetry/sender.rs` — refactor to use `spawn_detached`

Replace the `Command::new().spawn()` call with `spawn::spawn_detached()`.

### 6. `lib/crates/fabro-util/src/telemetry/panic.rs` — NEW FILE

Core logic:

- `install_panic_hook()` — chains onto default hook (like qlty). Calls `report_panic()` then delegates to default.
- `report_panic(info: &PanicHookInfo)` — checks telemetry level (Off → return), filters "Broken pipe" panics, builds `sentry::protocol::Event` with:
  - Exception with `mechanism.ty = "panic"`, `handled = false`
  - Panic message as `value`
  - `sentry_backtrace::current_stacktrace()`
  - OS context
  - Level = Fatal
- `spawn_panic_sender(event)` — serialize event to `~/.fabro/tmp/fabro-panic-{id}.json`, double-fork `fabro __send_panic <path>` with `FABRO_TELEMETRY=off`.
- `pub async fn send_panic_to_sentry(path: &Path)` — read JSON, init sentry client with `SENTRY_DSN` (compile-time `option_env!`), `sentry::capture_event()`, delete temp file.

### 7. `lib/crates/fabro-cli/src/main.rs` — two changes

**a) Install panic hook early in `main()`** (before `main_inner()`):
```rust
fabro_util::telemetry::panic::install_panic_hook();
```

**b) Add `__send_panic` hidden subcommand** (same pattern as `__send_analytics`).

## Key design decisions

- **Double-fork for all background senders** — ensures subprocess survives parent exit and terminal close. Fixes existing `__send_analytics` reliability too.
- **Shared `spawn_detached`** — avoids duplicating the fork logic between analytics and panic senders.
- **Telemetry level respected** — `Off` skips panic reporting. `Errors` and `All` both report.
- **Recursion prevention** — subprocess sets `FABRO_TELEMETRY=off`.
- **Compile-time DSN** — `option_env!("SENTRY_DSN")`, no-ops when unset (dev builds).
- **Filter broken pipe** — CLI tools get SIGPIPE from `| head` etc., not a real bug.

## Implementation order (red/green TDD)

Work bottom-up through the dependency chain. For each step, write the test first (red), then write the minimal code to make it pass (green).

### Step 1: `spawn_detached` (fabro-util)
- **Red**: Test that `spawn_detached` constructs the right args (unit-testable parts: temp file writing, arg assembly). On Unix, test the double-fork integration by spawning a real process that writes a marker file.
- **Green**: Implement `spawn.rs` with fork/setsid/close_fd/fork/exec on Unix, DETACHED_PROCESS on Windows.

### Step 2: Refactor `sender.rs` to use `spawn_detached`
- **Red**: Existing `send_noops_without_write_key` test still passes. Add test verifying temp file is written with correct JSON.
- **Green**: Replace `Command::spawn()` with `spawn_detached()`.

### Step 3: `panic.rs` — event building
- **Red**: Test `build_panic_event()` produces correct Sentry event structure (exception type, mechanism, level, stacktrace present).
- **Green**: Implement event builder.

### Step 4: `panic.rs` — hook + spawn
- **Red**: Test that `report_panic` filters broken pipe. Test that it no-ops when telemetry is Off.
- **Green**: Implement `install_panic_hook()`, `report_panic()`, `spawn_panic_sender()`.

### Step 5: `panic.rs` — Sentry sender
- **Red**: Test `send_panic_to_sentry` no-ops without DSN (like analytics no-ops without write key).
- **Green**: Implement the `__send_panic` worker function.

### Step 6: Wire up in `main.rs`
- Add `__send_panic` subcommand + install panic hook.
- Verify: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`


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
  - Model: claude-opus-4-6, 89.1k tokens in / 13.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/main.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/mod.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/panic.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/sender.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/spawn.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 60.2k tokens in / 17.7k out
  - Files: /home/daytona/workspace/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-util/Cargo.toml, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/panic.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/sender.rs, /home/daytona/workspace/lib/crates/fabro-util/src/telemetry/spawn.rs


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