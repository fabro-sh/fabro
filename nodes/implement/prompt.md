Goal: # Fix: Sub-agent file writes not tracked in API backend

## Context

When the API backend's agent spawns sub-agents that call `edit_file`/`write_file`, those files are missing from `outcome.files_touched`. This causes downstream nodes (like `simplify_opus`) to receive an incomplete file list in their prompt preamble.

Root cause: `spawn_event_forwarder` in `backend.rs` only matches top-level `ToolCallStarted`/`ToolCallCompleted` events. Sub-agent tool calls arrive wrapped as `AgentEvent::SubAgentEvent { event: Box<inner> }` and hit the `_ => {}` catch-all.

## Plan

**Single file change:** `lib/crates/fabro-workflows/src/cli/backend.rs`

In `spawn_event_forwarder` (line 51), replace the flat match with a helper that recursively unwraps `SubAgentEvent` to extract the inner `ToolCallStarted`/`ToolCallCompleted`:

```
match &event.event {
    AgentEvent::ToolCallStarted { .. } => { /* existing logic */ }
    AgentEvent::ToolCallCompleted { .. } => { /* existing logic */ }
+   AgentEvent::SubAgentEvent { event: inner, .. } => {
+       // Recursively extract file-tracking events from sub-agents
+       track_file_event(inner, &pending_tool_calls, &files_touched, &last_file_touched);
+   }
    _ => {}
}
```

Extract the file-tracking logic into a `track_file_event(event, pending, touched, last)` function that:
1. Matches `ToolCallStarted` for `write_file`/`edit_file` → records to `pending_tool_calls`
2. Matches `ToolCallCompleted` (non-error) → moves from pending to `files_touched`
3. Matches `SubAgentEvent` → recurses into the inner event (handles sub-sub-agents)
4. Otherwise → no-op

The existing inline match in `spawn_event_forwarder` calls `track_file_event` for both top-level and sub-agent events.

## Verification

1. `cargo test -p fabro-workflows` — existing tests pass
2. `cargo clippy --workspace -- -D warnings` — clean
3. Add a unit test: emit a `SubAgentEvent` wrapping a `ToolCallStarted`/`ToolCallCompleted` for `edit_file`, verify it appears in `files_touched`


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