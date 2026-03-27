# Fabro Events Strategy

Fabro emits structured **workflow run events** during execution for observability. Events write to `progress.jsonl` (one JSON object per line) and `live.json` (latest event snapshot) inside the run's log directory. Events are the primary record of what happened during a run — they feed the retro system, CLI verbose output, `fabro attach`, `fabro logs`, and live monitoring.

Events are distinct from tracing logs (see `logging-strategy.md`). Tracing is developer diagnostics; events are the structured audit trail consumed by tooling.

Detached runs rely on this distinction. If a warning or error needs to be visible to users after reattach, emit a `WorkflowRunEvent` rather than only writing to stderr/`detach.log`.

## Architecture

```
Engine/Handler → WorkflowRunEvent enum → EventEmitter
                                            ├─ .trace()        → tracing log line (automatic)
                                            ├─ flatten_event()  → progress.jsonl + live.json
                                            └─ on_event() callbacks → CLI output, cost tracking, etc.
```

**Three layers:**

1. **Rust enum** (`WorkflowRunEvent` in `event.rs`) — the source of truth for event structure. Variants use Rust naming and types. `AgentEvent` and `SandboxEvent` from `fabro-agent` are wrapped as `Agent { stage, event }` and `Sandbox { event }`.

2. **Flattening** (`flatten_event()` in `event.rs`) — serializes the enum via serde, then restructures nested/tagged variants into `(event_name, flat_fields_map)`. Wrapper variants use dot notation: `Agent.ToolCallStarted`, `Sandbox.Initializing`.

3. **Field renaming** (`rename_fields()` in `event.rs`) — post-processes the flat fields to give them self-describing names for JSONL output. This avoids changing the Rust enum while making the external format unambiguous.

## JSONL Envelope

Every line in `progress.jsonl` has three envelope fields, then the event's own fields merged at the top level:

```json
{"ts":"2025-06-15T12:00:00.123Z","run_id":"01J...","event":"StageCompleted","node_id":"plan","node_label":"Plan","stage_index":0,"duration_ms":5000,"status":"success",...}
```

| Envelope field | Type | Description |
|---|---|---|
| `ts` | ISO 8601 string | UTC timestamp with millisecond precision |
| `run_id` | string | ULID for this workflow run |
| `event` | string | Event name (matches Rust variant, dot-separated for wrapped types) |

The envelope is built in `fabro-workflows/src/event.rs` by `build_event_envelope()`, and file logging is handled by `ProgressLogger`. Field names from the event that collide with envelope keys (`ts`, `run_id`, `event`) are dropped — the `run_id` from `WorkflowRunStarted` populates the envelope itself.

## Run Completion Contract

`status.json` is the authoritative completion signal for detached runs. Write a terminal `RunStatus` only after all post-run work is complete, including retro generation, finalize-commit work, pull request creation, and sandbox cleanup. `conclusion.json` should be written at that same final point so `attach`, `wait`, and summary rendering all agree on when the run is actually finished.

## Node Terminology

- **`node_id`** — programmatic identifier (the id from the DOT graph). Stable, used for matching.
- **`node_label`** — display name (from the DOT `label` attribute, defaults to `node_id`). Human-readable.

Every event that references a graph node should include both. Stage events carry both from the Rust enum. Events that only have an id (Agent, ParallelBranch, etc.) get `node_label` defaulted to `node_id` by `rename_fields()`.

## Field Naming Conventions

### Rules

1. **Self-describing** — a field name should be unambiguous without knowing the event type. Use `node_id` not `name`, `stage_index` not `index`, `sandbox_provider` not `provider`.
2. **`_id` suffix** for identifiers — `node_id`, `from_node_id`, `to_node_id`, `start_node_id`, `tool_call_id`, `agent_id`.
3. **`_ms` suffix** for durations — `duration_ms`, `delay_ms`. Always milliseconds.
4. **`_count` suffix** for counts — `branch_count`, `command_count`, `tool_call_count`.
5. **No prefix** for fields that are already unambiguous — `error`, `status`, `command`, `question`, `answer`, `model`.
6. **`snake_case`** for all field names.

### Rename Table (Rust enum → JSONL)

The Rust enum uses short field names for ergonomics. `rename_fields()` transforms them for the JSONL output:

| Rust field | JSONL field | Events | Reason |
|---|---|---|---|
| `name` | `workflow_name` | WorkflowRunStarted | Disambiguate |
| `name` | `node_label` | Stage* | Display name |
| `name` | `snapshot_name` | Sandbox.Snapshot* | Disambiguate |
| `index` | `stage_index` | Stage* | Disambiguate |
| `index` | `branch_index` | ParallelBranch* | Disambiguate |
| `index` | `command_index` | SetupCommand*, SetupFailed, DevcontainerLifecycleCommand*, DevcontainerLifecycleFailed | Disambiguate |
| `stage` | `node_id` | Agent.*, Interview*, Prompt | Unify terminology |
| `branch` | `node_id` | ParallelBranch* | Consistent |
| `node` | `node_id` | StallWatchdogTimeout | Consistent |
| `from_node` | `from_node_id` | EdgeSelected, LoopRestart | `_id` suffix |
| `to_node` | `to_node_id` | EdgeSelected, LoopRestart | `_id` suffix |
| `start_node` | `start_node_id` | SubgraphStarted | `_id` suffix |
| `provider` | `sandbox_provider` | Sandbox.* | Disambiguate |
| `text` | `prompt_text` | Prompt | Disambiguate |
| _(inserted)_ | `node_label` | Agent.*, ParallelBranch*, etc. | Defaults to `node_id` |

Fields not in this table pass through unchanged.

## Adding a New Event

### Step 1: Add to the Rust enum

Add a variant to `WorkflowRunEvent` in `event.rs`. Use the short Rust field names (they'll be renamed in step 3).

```rust
MyNewEvent {
    node_id: String,     // if it references a graph node
    name: String,        // if it has a display label (will become node_label)
    duration_ms: u64,
    // ...
},
```

For events that wrap `AgentEvent` or `SandboxEvent`, add the variant to those enums in `fabro-agent` instead — they're automatically wrapped by the existing `Agent { stage, event }` and `Sandbox { event }` variants.

### Step 2: Add a trace() match arm

Add a match arm in `WorkflowRunEvent::trace()`. Choose the tracing level per `logging-strategy.md`:
- INFO for lifecycle boundaries (started/completed at the workflow level)
- DEBUG for individual steps (stage started, tool call, etc.)
- WARN for retries and degraded behavior
- ERROR for terminal failures

```rust
Self::MyNewEvent { node_id, duration_ms, .. } => {
    debug!(node_id, duration_ms, "My new event happened");
}
```

### Step 3: Add rename rules (if needed)

If your event has fields that need renaming (ambiguous `name`, `index`, `stage`, etc.), add a branch in `rename_fields()` in `event.rs`. If your event references a graph node and only has `node_id`, call `default_node_label(fields)` to insert `node_label`.

### Step 4: Emit from engine or handler

Emit via the `EventEmitter`:

```rust
self.services.emitter.emit(&WorkflowRunEvent::MyNewEvent {
    node_id: node.id.clone(),
    name: node.label().to_string(),
    duration_ms: elapsed,
});
```

### Step 5: Update format_event_summary

Add a match arm in `format_event_summary()` in `cli/mod.rs` for `-v` verbose output:

```rust
WorkflowRunEvent::MyNewEvent { node_id, duration_ms, .. } => {
    format!("[MY_NEW_EVENT] node_id={node_id} duration={duration_ms}ms")
}
```

### Step 6: Update tests

- Add a serialization test in `event.rs` (serde round-trip)
- Add a `rename_fields` test if you added rename rules
- Update integration test patterns in `integration.rs` if matching on the new event

## Complete Event Reference

### Workflow lifecycle

| Event | JSONL fields |
|---|---|
| `WorkflowRunStarted` | `workflow_name`, `run_id`, `base_branch`?, `base_sha`?, `run_branch`?, `worktree_dir`? |
| `WorkflowRunCompleted` | `duration_ms`, `artifact_count`, `total_cost`?, `final_git_commit_sha`? |
| `WorkflowRunFailed` | `error`, `duration_ms`, `git_commit_sha`? |
| `RunNotice` | `level`, `code`, `message` |

### Stage execution

| Event | JSONL fields |
|---|---|
| `StageStarted` | `node_id`, `node_label`, `stage_index`, `handler_type`?, `attempt`, `max_attempts` |
| `StageCompleted` | `node_id`, `node_label`, `stage_index`, `duration_ms`, `status`, `preferred_label`?, `suggested_next_ids`, `usage`?, `failure_reason`?, `notes`?, `files_touched`, `attempt`, `max_attempts`, `failure_class`? |
| `StageFailed` | `node_id`, `node_label`, `stage_index`, `error`, `will_retry`, `failure_reason`?, `failure_class`? |
| `StageRetrying` | `node_id`, `node_label`, `stage_index`, `attempt`, `max_attempts`, `delay_ms` |

### Parallel execution

| Event | JSONL fields |
|---|---|
| `ParallelStarted` | `branch_count`, `join_policy` |
| `ParallelBranchStarted` | `node_id`, `node_label`, `branch_index` |
| `ParallelBranchCompleted` | `node_id`, `node_label`, `branch_index`, `duration_ms`, `status` |
| `ParallelCompleted` | `duration_ms`, `success_count`, `failure_count` |

### Graph navigation

| Event | JSONL fields |
|---|---|
| `EdgeSelected` | `from_node_id`, `to_node_id`, `label`?, `condition`? |
| `LoopRestart` | `from_node_id`, `to_node_id` |
| `SubgraphStarted` | `node_id`, `node_label`, `start_node_id` |
| `SubgraphCompleted` | `node_id`, `node_label`, `steps_executed`, `status`, `duration_ms` |

### Checkpoints and git

| Event | JSONL fields |
|---|---|
| `CheckpointCompleted` | `node_id`, `node_label`, `status`, `git_commit_sha` (optional) |
| `CheckpointFailed` | `node_id`, `node_label`, `error` |
| `GitCommit` | `node_id` (optional), `node_label` (optional), `sha` |
| `GitPush` | `branch`, `success` |
| `GitBranch` | `branch`, `sha` |
| `GitWorktreeAdd` | `path`, `branch` |
| `GitWorktreeRemove` | `path` |
| `GitFetch` | `branch`, `success` |
| `GitReset` | `sha` |

### Human interaction

| Event | JSONL fields |
|---|---|
| `InterviewStarted` | `question`, `node_id`, `node_label`, `question_type` |
| `InterviewCompleted` | `question`, `answer`, `duration_ms` |
| `InterviewTimeout` | `question`, `node_id`, `node_label`, `duration_ms` |
| `Prompt` | `node_id`, `node_label`, `prompt_text` |

### Setup

| Event | JSONL fields |
|---|---|
| `SetupStarted` | `command_count` |
| `SetupCommandStarted` | `command`, `command_index` |
| `SetupCommandCompleted` | `command`, `command_index`, `exit_code`, `duration_ms` |
| `SetupCompleted` | `duration_ms` |
| `SetupFailed` | `command`, `command_index`, `exit_code`, `stderr` |

### Devcontainer

| Event | JSONL fields |
|---|---|
| `DevcontainerResolved` | `dockerfile_lines`, `environment_count`, `lifecycle_command_count`, `workspace_folder` |
| `DevcontainerLifecycleStarted` | `phase`, `command_count` |
| `DevcontainerLifecycleCommandStarted` | `phase`, `command`, `command_index` |
| `DevcontainerLifecycleCommandCompleted` | `phase`, `command`, `command_index`, `exit_code`, `duration_ms` |
| `DevcontainerLifecycleCompleted` | `phase`, `duration_ms` |
| `DevcontainerLifecycleFailed` | `phase`, `command`, `command_index`, `exit_code`, `stderr` |

### Stall detection

| Event | JSONL fields |
|---|---|
| `StallWatchdogTimeout` | `node_id`, `node_label`, `idle_seconds` |

### Agent events (prefixed `Agent.`)

All agent events include `node_id` and `node_label`.

| Event | Additional JSONL fields |
|---|---|
| `Agent.SessionStarted` | _(none)_ |
| `Agent.SessionEnded` | _(none)_ |
| `Agent.UserInput` | `text` |
| `Agent.AssistantTextStart` | _(none)_ |
| `Agent.AssistantMessage` | `text`, `model`, `usage` (object), `tool_call_count` |
| `Agent.TextDelta` | `delta` |
| `Agent.ToolCallStarted` | `tool_name`, `tool_call_id`, `arguments` |
| `Agent.ToolCallOutputDelta` | `delta` |
| `Agent.ToolCallCompleted` | `tool_name`, `tool_call_id`, `output`, `is_error` |
| `Agent.Error` | `error` |
| `Agent.Warning` | `kind`, `message`, `details` |
| `Agent.LoopDetected` | _(none)_ |
| `Agent.TurnLimitReached` | `max_turns` |
| `Agent.SkillExpanded` | `skill_name` |
| `Agent.SteeringInjected` | `text` |
| `Agent.CompactionStarted` | `estimated_tokens`, `context_window_size` |
| `Agent.CompactionCompleted` | `original_turn_count`, `preserved_turn_count`, `summary_token_estimate`, `tracked_file_count` |
| `Agent.LlmRetry` | `provider`, `model`, `attempt`, `delay_secs`, `error` |
| `Agent.SubAgentSpawned` | `agent_id`, `depth`, `task` |
| `Agent.SubAgentCompleted` | `agent_id`, `depth`, `success`, `turns_used` |
| `Agent.SubAgentFailed` | `agent_id`, `depth`, `error` |
| `Agent.SubAgentClosed` | `agent_id`, `depth` |
| `Agent.SubAgentEvent.*` | `agent_id`, `depth`, `nested_event` (JSON) |
| `Agent.McpServerReady` | `server_name`, `tool_count` |
| `Agent.McpServerFailed` | `server_name`, `error` |

### Sandbox events (prefixed `Sandbox.`)

| Event | JSONL fields |
|---|---|
| `Sandbox.Initializing` | `sandbox_provider` |
| `Sandbox.Ready` | `sandbox_provider`, `duration_ms` |
| `Sandbox.InitializeFailed` | `sandbox_provider`, `error`, `duration_ms` |
| `Sandbox.CleanupStarted` | `sandbox_provider` |
| `Sandbox.CleanupCompleted` | `sandbox_provider`, `duration_ms` |
| `Sandbox.CleanupFailed` | `sandbox_provider`, `error` |
| `Sandbox.SnapshotPulling` | `snapshot_name` |
| `Sandbox.SnapshotPulled` | `snapshot_name`, `duration_ms` |
| `Sandbox.SnapshotEnsuring` | `snapshot_name` |
| `Sandbox.SnapshotCreating` | `snapshot_name` |
| `Sandbox.SnapshotReady` | `snapshot_name`, `duration_ms` |
| `Sandbox.SnapshotFailed` | `snapshot_name`, `error` |
| `Sandbox.GitCloneStarted` | `url`, `branch` |
| `Sandbox.GitCloneCompleted` | `url`, `duration_ms` |
| `Sandbox.GitCloneFailed` | `url`, `error` |

## Error Fields

Error information is stored as plain strings. The `error` field contains the human-readable message; `failure_class` contains the machine-readable classification.

| Field | Type | Events | Purpose |
|---|---|---|---|
| `error` | string | StageFailed, WorkflowRunFailed, Agent.Error, Agent.LlmRetry, Sandbox.*Failed, etc. | Human-readable error message |
| `failure_reason` | string? | StageFailed, StageCompleted | Outcome-level failure description |
| `failure_class` | string? | StageFailed, StageCompleted | Machine classification: `transient_infra`, `deterministic`, `budget_exhausted`, `compilation_loop`, `canceled`, `structural` |

`failure_class` is derived from `FabroError::failure_class()` for handler errors, or from handler hints in `context_updates["failure_class"]` for outcome-based failures. See `error.rs` for the classification logic.

## Consumers

| Consumer | Reads | Purpose |
|---|---|---|
| `retro.rs` `extract_stage_durations()` | `node_label`, `duration_ms` from `StageCompleted` | Build retro report |
| `cli/run.rs` non-verbose listener | `name`, `duration_ms`, `status`, `usage` from `StageCompleted/Failed` | CLI progress output |
| `cli/mod.rs` `format_event_summary()` | All events | `-v` verbose output |
| `cli/run.rs` cost accumulator | `usage` from `StageCompleted` | Total cost tracking |
| `cli/run.rs` git SHA tracker | `git_commit_sha` from `CheckpointCompleted` | Final SHA for `conclusion.json` |
| External tooling | `progress.jsonl` | Live monitoring, dashboards |
