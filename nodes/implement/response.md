All changes are complete. Here's a summary of what was done:

### Changes Made

**1. `lib/crates/fabro-workflows/src/event.rs`** — Added `goal` field to `WorkflowRunStarted`
- Added `goal: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` for backward compatibility
- Updated all 3 test construction sites to include `goal: None`
- Added 3 new tests:
  - `workflow_run_started_with_goal_roundtrip` — serializes/deserializes with `goal: Some("Fix the bug")`
  - `workflow_run_started_without_goal_backward_compat` — old JSONL without `goal` deserializes to `goal: None`
  - `workflow_run_started_none_goal_omitted_from_json` — `goal: None` is not included in serialized output

**2. `lib/crates/fabro-workflows/src/engine.rs`** — Populated `goal` at emit site
- Uses `graph.goal()` to populate the field: `None` when empty, `Some(...)` otherwise

**3. `lib/crates/fabro-workflows/src/cli/logs.rs`** — Rendered goal in pretty logs
- When `goal` is present, renders it below the header line using `render_markdown_width()` with proper indentation (same approach as `Agent.AssistantMessage`)
- When `goal` is absent, renders single-line header as before
- Added 2 new tests:
  - Updated `pretty_workflow_run_started` to verify no newline when no goal
  - Added `pretty_workflow_run_started_with_goal` to verify goal appears on indented line