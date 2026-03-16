Everything looks correct. Here's a summary of what was done:

## Changes Made

**File:** `lib/crates/fabro-workflows/src/cli/backend.rs`

### 1. Extracted `track_file_event` helper function (lines 33–75)

A new recursive function that handles file-tracking logic for `write_file`/`edit_file` tool calls:
- **`ToolCallStarted`** → records the `file_path` argument in `pending_tool_calls` keyed by `tool_call_id`
- **`ToolCallCompleted`** (non-error) → moves the path from `pending_tool_calls` to `files_touched` and updates `last_file_touched`
- **`ToolCallCompleted`** (error) → removes from `pending_tool_calls` without recording
- **`SubAgentEvent`** → recursively unwraps the inner event and calls itself (handles arbitrarily nested sub-agents)
- Everything else → no-op

### 2. Refactored `spawn_event_forwarder` to use `track_file_event`

Replaced the inline `match` block with a single call to `track_file_event(&event.event, ...)`, which now handles both top-level and sub-agent events.

### 3. Added 4 unit tests

- `track_file_event_records_top_level_write` — verifies basic `write_file` tracking still works
- `track_file_event_unwraps_sub_agent_edit` — verifies `SubAgentEvent`-wrapped `edit_file` calls are tracked
- `track_file_event_unwraps_nested_sub_sub_agent` — verifies double-nested `SubAgentEvent` (sub-sub-agent) is handled
- `track_file_event_error_removes_pending` — verifies errored sub-agent tool calls don't pollute `files_touched`

### Verification
- ✅ `cargo test -p fabro-workflows` — 179 passed, 0 failed
- ✅ `cargo clippy --workspace -- -D warnings` — clean