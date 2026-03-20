Goal: # Emit `StageStarted` on retry attempts

## Context

When a stage fails with a transient error and is retried, the CLI progress UI freezes because:

1. `StageFailed` calls `finish_stage()`, removing the stage from `active_stages`
2. The retry loop in the engine (`continue` at line 1198) re-enters handler execution **without emitting `StageStarted`**
3. All subsequent agent events for the retry attempt silently drop (no matching entry in `active_stages`)

The `StageStarted` event already has `attempt` and `max_attempts` fields, so emitting it per-attempt is the intended design — it just wasn't wired up.

## Changes

### 1. Engine: emit `StageStarted` at the top of the retry loop

**File:** `lib/crates/fabro-workflows/src/engine.rs`

Move the `StageStarted` emission from before the loop (line 1852) to inside the loop, right after `for attempt in 1..=policy.max_attempts {` (line 1079). This way every attempt — including retries — emits the event with the correct `attempt` number.

The existing emission at line 1852 gets replaced, not duplicated. The `attempt` value comes directly from the loop variable (converted via `usize::try_from`).

### 2. Engine: move StageStart hook inside the loop (or keep it outside)

The `StageStart` hook block (lines 1862-1895) currently runs once before the loop. It should stay outside — hooks shouldn't re-fire on retries. Only the `StageStarted` event emission moves inside.

### 3. UI: no changes needed

`on_stage_started` in `run_progress.rs` already handles being called for the same `node_id` — it inserts a fresh `ActiveStage` into the map, creating a new spinner. The `StageFailed` handler correctly finishes the old spinner. The natural event sequence becomes:

```
StageStarted  (attempt 1) → spinner created
StageFailed   (will_retry) → spinner finished with ✗
StageStarted  (attempt 2) → new spinner created
Agent events  → attach to new spinner
StageCompleted (attempt 2) → spinner finished with ✓
```

## Verification

1. `cargo test -p fabro-workflows` — existing tests pass
2. `cargo clippy --workspace -- -D warnings` — no warnings
3. Manual: run a workflow that hits a transient LLM error (or mock one) and verify the CLI shows the retry spinner with tool calls


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