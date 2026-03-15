All checks pass. Here's a summary of the changes made:

## Summary of Changes

### 1. `lib/crates/fabro-workflows/src/event.rs`
- **Added `goal` field** to `WorkflowRunEvent::WorkflowRunStarted` with `#[serde(default, skip_serializing_if = "Option::is_none")]` for backward compatibility with old JSONL files (same pattern as `base_sha`, `run_branch`, `worktree_dir`)
- **Updated 3 test construction sites** to include `goal: None`:
  - `emitter_on_event_receives_events` test
  - `emitter_last_event_at_updates_after_emit` test
  - `rename_fields_workflow_run_started` test
- **Added 3 new tests**:
  - `workflow_run_started_with_goal_round_trip` — verifies serialization/deserialization with `goal: Some("Fix the bug")`
  - `workflow_run_started_without_goal_backward_compat` — verifies old JSONL without `goal` field deserializes to `goal: None`
  - `workflow_run_started_goal_none_omitted_from_json` — verifies `goal: None` is omitted via `skip_serializing_if`

### 2. `lib/crates/fabro-workflows/src/engine.rs`
- **Populated `goal` at the emit site** using `graph.goal()`, emitting `None` when empty and `Some(...)` otherwise

### 3. `lib/crates/fabro-workflows/src/cli/logs.rs`
- **Updated `format_event_pretty`** for `WorkflowRunStarted`: when `goal` is present and non-empty, renders it below the header line using `styles.render_markdown_width()` with indentation (same approach as `Agent.AssistantMessage`). When no goal is present, renders just the header line as before.
- **Added 2 new tests**:
  - `pretty_workflow_run_started_with_goal` — verifies goal text appears in output with multi-line format
  - `pretty_workflow_run_started_without_goal_no_extra_lines` — verifies no extra newlines when goal is absent