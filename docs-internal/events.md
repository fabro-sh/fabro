# Events

Every event in `progress.jsonl` is a JSON object with this envelope structure:

```json
{
  "id": "019234ab-cdef-7890-abcd-ef1234567890",
  "ts": "2026-04-01T12:00:00.123Z",
  "run_id": "01JQXYZ...",
  "event": "stage.completed",
  "session_id": "ses_abc",
  "parent_session_id": "ses_parent",
  "node_id": "code",
  "node_label": "Write Code",
  "properties": { ... }
}
```

### Envelope fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | UUID v7 (time-ordered), unique per event |
| `ts` | string | RFC 3339 timestamp with millisecond precision |
| `run_id` | string | ULID of the run |
| `event` | string | Dot-notation event name |
| `session_id` | string? | Agent session id (agent events only) |
| `parent_session_id` | string? | Parent agent session id (agent events only) |
| `node_id` | string? | Node id (stage, checkpoint, agent, parallel branch, and other node-scoped events) |
| `node_label` | string? | Display label for the node (defaults to `node_id` when not set separately) |
| `properties` | object | Event-specific fields |

---

## Run events

### `run.started`

Emitted when the workflow run begins.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "run.started",
  "properties": {
    "name": "my-workflow",
    "base_branch": "main",
    "base_sha": "abc123...",
    "run_branch": "fabro/run-01JQXYZ",
    "worktree_dir": "/tmp/fabro-worktrees/...",
    "goal": "Fix the login bug"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Workflow name |
| `base_branch` | string? | Base git branch |
| `base_sha` | string? | Base commit SHA |
| `run_branch` | string? | Git branch created for this run |
| `worktree_dir` | string? | Worktree directory path |
| `goal` | string? | Workflow goal text |

Note: `run_id` is in the envelope, not in properties.

### `run.completed`

Emitted when the workflow run finishes successfully (or with partial success).

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "run.completed",
  "properties": {
    "duration_ms": 45000,
    "artifact_count": 3,
    "status": "success",
    "total_cost": 0.15,
    "final_git_commit_sha": "def456...",
    "usage": {
      "input_tokens": 15000,
      "output_tokens": 5000,
      "total_tokens": 20000,
      "reasoning_tokens": 2000,
      "cache_read_tokens": 8000,
      "cache_write_tokens": 3000,
      "speed": "standard"
    }
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `duration_ms` | number | Total run duration in milliseconds |
| `artifact_count` | number | Number of artifacts produced |
| `status` | string | Final status (`"success"`, `"fail"`, `"partial_success"`) |
| `total_cost` | number? | Aggregate cost in USD |
| `final_git_commit_sha` | string? | Final HEAD SHA |
| `usage` | object? | Aggregate token usage |
| `usage.input_tokens` | number | Total input tokens |
| `usage.output_tokens` | number | Total output tokens |
| `usage.total_tokens` | number | Total tokens (input + output) |
| `usage.reasoning_tokens` | number? | Total reasoning/thinking tokens |
| `usage.cache_read_tokens` | number? | Total cache read tokens |
| `usage.cache_write_tokens` | number? | Total cache write tokens |
| `usage.speed` | string? | Speed tier |
| `usage.raw` | object? | Raw provider-specific usage data |

### `run.failed`

Emitted when the workflow run fails.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "run.failed",
  "properties": {
    "error": "Handler error: compilation failed",
    "duration_ms": 12000,
    "git_commit_sha": "abc123..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `error` | string | Error message (Display representation) |
| `duration_ms` | number | Run duration before failure |
| `git_commit_sha` | string? | HEAD SHA at time of failure |

### `run.notice`

Informational, warning, or error notice emitted during the run.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "run.notice",
  "properties": {
    "level": "warn",
    "code": "missing_env_var",
    "message": "GITHUB_TOKEN not set, PR creation will be skipped"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `level` | string | `"info"`, `"warn"`, or `"error"` |
| `code` | string | Machine-readable notice code |
| `message` | string | Human-readable message |

---

## Stage events

### `stage.started`

Emitted when a workflow node begins execution.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "stage.started",
  "node_id": "code",
  "node_label": "Write Code",
  "properties": {
    "index": 1,
    "handler_type": "agent",
    "script": null,
    "attempt": 1,
    "max_attempts": 3
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Stage execution order index |
| `handler_type` | string? | Handler type (`"agent"`, `"prompt"`, `"command"`, `"conditional"`, `"human"`, `"parallel"`, etc.) |
| `script` | string? | Script body (command nodes only) |
| `attempt` | number | Current attempt number (1-based) |
| `max_attempts` | number | Maximum attempts allowed |

### `stage.completed`

Emitted when a workflow node finishes execution.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "stage.completed",
  "node_id": "code",
  "node_label": "Write Code",
  "properties": {
    "index": 1,
    "duration_ms": 8000,
    "status": "success",
    "preferred_label": "tests_pass",
    "suggested_next_ids": ["review"],
    "usage": {
      "model": "claude-sonnet-4-20250514",
      "input_tokens": 5000,
      "output_tokens": 2000,
      "cache_read_tokens": 3000,
      "cache_write_tokens": 1000,
      "reasoning_tokens": 500,
      "speed": "standard",
      "cost": 0.05
    },
    "error": "lint failed",
    "failure_class": "deterministic",
    "failure_signature": "clippy::unused_import",
    "notes": "All tests passing",
    "files_touched": ["src/main.rs", "src/lib.rs"],
    "attempt": 1,
    "max_attempts": 3
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Stage execution order index |
| `duration_ms` | number | Stage duration in milliseconds |
| `status` | string | `"success"`, `"fail"`, `"skipped"`, `"partial_success"`, `"retry"` |
| `preferred_label` | string? | Edge label hint for routing |
| `suggested_next_ids` | string[] | Suggested successor node ids |
| `usage` | object? | Token usage for this stage |
| `usage.model` | string | Model identifier |
| `usage.input_tokens` | number | Input tokens |
| `usage.output_tokens` | number | Output tokens |
| `usage.cache_read_tokens` | number? | Cache read tokens |
| `usage.cache_write_tokens` | number? | Cache write tokens |
| `usage.reasoning_tokens` | number? | Reasoning/thinking tokens |
| `usage.speed` | string? | Speed tier |
| `usage.cost` | number? | Estimated cost in USD |
| `error` | string? | Error message (flattened from failure detail) |
| `failure_class` | string? | `"transient_infra"`, `"deterministic"`, `"budget_exhausted"`, `"compilation_loop"`, `"canceled"`, `"structural"` |
| `failure_signature` | string? | Dedup key for repeated failures |
| `notes` | string? | Free-text notes |
| `files_touched` | string[] | File paths modified |
| `attempt` | number | Attempt number (1-based) |
| `max_attempts` | number | Maximum attempts allowed |

Note: `failure` is flattened — the `failure.message` becomes `error`, `failure.failure_class` becomes `failure_class`, `failure.failure_signature` becomes `failure_signature`.

### `stage.failed`

Emitted when a stage fails (before retry decision).

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "stage.failed",
  "node_id": "code",
  "node_label": "Write Code",
  "properties": {
    "index": 1,
    "error": "compilation failed",
    "failure_class": "deterministic",
    "failure_signature": "rustc::E0308",
    "will_retry": true
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Stage execution order index |
| `error` | string | Error message (flattened from failure detail) |
| `failure_class` | string | Failure category |
| `failure_signature` | string? | Dedup key for repeated failures |
| `will_retry` | boolean | Whether the stage will be retried |

### `stage.retrying`

Emitted when a stage is about to be retried.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "stage.retrying",
  "node_id": "code",
  "node_label": "Write Code",
  "properties": {
    "index": 1,
    "attempt": 2,
    "max_attempts": 3,
    "delay_ms": 1000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Stage execution order index |
| `attempt` | number | Next attempt number |
| `max_attempts` | number | Maximum attempts allowed |
| `delay_ms` | number | Delay before retry in milliseconds |

### `stage.prompt`

Emitted when a prompt is rendered for an LLM stage.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "stage.prompt",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "text": "You are a coding agent. Fix the bug in..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `text` | string | Rendered prompt text |

---

## Parallel events

### `parallel.started`

Emitted when a parallel node begins executing branches.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "parallel.started",
  "properties": {
    "branch_count": 3,
    "join_policy": "all"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `branch_count` | number | Number of parallel branches |
| `join_policy` | string | Join policy |

### `parallel.branch.started`

Emitted when a parallel branch begins.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "parallel.branch.started",
  "node_id": "branch_a",
  "node_label": "branch_a",
  "properties": {
    "index": 0
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Branch index |

### `parallel.branch.completed`

Emitted when a parallel branch finishes.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "parallel.branch.completed",
  "node_id": "branch_a",
  "node_label": "branch_a",
  "properties": {
    "index": 0,
    "duration_ms": 5000,
    "status": "success"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `index` | number | Branch index |
| `duration_ms` | number | Branch duration in milliseconds |
| `status` | string | Branch outcome status |

### `parallel.completed`

Emitted when all parallel branches have finished.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "parallel.completed",
  "properties": {
    "duration_ms": 12000,
    "success_count": 2,
    "failure_count": 1
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `duration_ms` | number | Total parallel duration |
| `success_count` | number | Branches that succeeded |
| `failure_count` | number | Branches that failed |

---

## Interview events

### `interview.started`

Emitted when a human-in-the-loop question is posed.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "interview.started",
  "node_id": "review",
  "node_label": "review",
  "properties": {
    "question": "Does this look correct?",
    "question_type": "approval"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `question` | string | Question text |
| `question_type` | string | Type of question |

### `interview.completed`

Emitted when a human answers.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "interview.completed",
  "properties": {
    "question": "Does this look correct?",
    "answer": "yes",
    "duration_ms": 30000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `question` | string | Question text |
| `answer` | string | Human's answer |
| `duration_ms` | number | Time waiting for answer |

### `interview.timeout`

Emitted when a human question times out.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "interview.timeout",
  "node_id": "review",
  "node_label": "review",
  "properties": {
    "question": "Does this look correct?",
    "duration_ms": 300000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `question` | string | Question text |
| `duration_ms` | number | Time waited before timeout |

---

## Checkpoint events

### `checkpoint.completed`

Emitted after a checkpoint is saved.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "checkpoint.completed",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "status": "success",
    "git_commit_sha": "abc123..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `status` | string | Checkpoint status |
| `git_commit_sha` | string? | Commit SHA at checkpoint time |

### `checkpoint.failed`

Emitted when checkpoint saving fails.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "checkpoint.failed",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "error": "git commit failed: ..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `error` | string | Error message |

---

## Git events

### `git.commit`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.commit",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "sha": "abc123..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `sha` | string | Commit SHA |

Note: `node_id` is optional — may be absent for non-stage commits.

### `git.push`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.push",
  "properties": {
    "branch": "fabro/run-01JQXYZ",
    "success": true
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `branch` | string | Branch name |
| `success` | boolean | Whether push succeeded |

### `git.branch`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.branch",
  "properties": {
    "branch": "fabro/run-01JQXYZ",
    "sha": "abc123..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `branch` | string | Branch name |
| `sha` | string | Branch HEAD SHA |

### `git.worktree.added`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.worktree.added",
  "properties": {
    "path": "/tmp/fabro-worktrees/...",
    "branch": "fabro/run-01JQXYZ"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `path` | string | Worktree directory path |
| `branch` | string | Branch name |

### `git.worktree.removed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.worktree.removed",
  "properties": {
    "path": "/tmp/fabro-worktrees/..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `path` | string | Worktree directory path |

### `git.fetch`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.fetch",
  "properties": {
    "branch": "main",
    "success": true
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `branch` | string | Branch name |
| `success` | boolean | Whether fetch succeeded |

### `git.reset`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "git.reset",
  "properties": {
    "sha": "abc123..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `sha` | string | Target commit SHA |

---

## Routing events

### `edge.selected`

Emitted when the engine selects the next edge to traverse.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "edge.selected",
  "properties": {
    "from_node": "code",
    "to_node": "review",
    "label": "tests_pass",
    "condition": "outcome=success",
    "reason": "condition",
    "preferred_label": "tests_pass",
    "suggested_next_ids": ["review"],
    "stage_status": "success",
    "is_jump": false
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `from_node` | string | Source node id |
| `to_node` | string | Target node id |
| `label` | string? | Edge label |
| `condition` | string? | Edge condition expression |
| `reason` | string | Selection reason (`"condition"`, `"preferred_label"`, `"jump"`, etc.) |
| `preferred_label` | string? | Stage's preferred label hint |
| `suggested_next_ids` | string[] | Stage's suggested next node ids |
| `stage_status` | string | Outcome status that influenced routing |
| `is_jump` | boolean | Whether this bypassed normal edge selection |

### `loop.restart`

Emitted when execution loops back to an earlier node.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "loop.restart",
  "properties": {
    "from_node": "review",
    "to_node": "code"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `from_node` | string | Node that triggered the restart |
| `to_node` | string | Node to restart from |

---

## Agent events

All agent events have `node_id` (the workflow stage), `node_label`, `session_id`, and `parent_session_id` in the envelope. The `properties` contain the inner agent event fields.

### `agent.session.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.session.started",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc", "parent_session_id": null,
  "properties": {}
}
```

No properties.

### `agent.session.ended`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.session.ended",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {}
}
```

No properties.

### `agent.processing.end`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.processing.end",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {}
}
```

No properties.

### `agent.input`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.input",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "text": "Fix the login bug in auth.rs"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `text` | string | User input text |

### `agent.output.start`

Signals the beginning of assistant text output.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.output.start",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {}
}
```

No properties.

### `agent.output.replace`

Replaces the current in-progress assistant output buffers.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.output.replace",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "text": "I'll fix the login bug by...",
    "reasoning": "The user wants..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `text` | string | Replacement assistant text |
| `reasoning` | string? | Replacement reasoning text |

### `agent.message`

Emitted when the assistant produces a complete message.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.message",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "text": "I've fixed the bug in auth.rs by...",
    "model": "claude-sonnet-4-20250514",
    "usage": {
      "input_tokens": 3000,
      "output_tokens": 1500,
      "total_tokens": 4500,
      "reasoning_tokens": 200,
      "cache_read_tokens": 1000,
      "cache_write_tokens": 500
    },
    "tool_call_count": 2
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `text` | string | Assistant message text |
| `model` | string | Model identifier |
| `usage` | object | Token usage for this message |
| `usage.input_tokens` | number | Input tokens |
| `usage.output_tokens` | number | Output tokens |
| `usage.total_tokens` | number | Total tokens |
| `usage.reasoning_tokens` | number? | Reasoning tokens |
| `usage.cache_read_tokens` | number? | Cache read tokens |
| `usage.cache_write_tokens` | number? | Cache write tokens |
| `usage.speed` | string? | Speed tier |
| `usage.raw` | object? | Raw provider-specific usage |
| `tool_call_count` | number | Number of tool calls in this turn |

### `agent.text.delta`

Streaming text chunk from the assistant.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.text.delta",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "delta": "I'll start by reading"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `delta` | string | Text chunk |

### `agent.reasoning.delta`

Streaming reasoning/thinking chunk from the assistant.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.reasoning.delta",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "delta": "The user needs me to..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `delta` | string | Reasoning text chunk |

### `agent.tool.started`

Emitted when the agent begins a tool call.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.tool.started",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "tool_name": "read_file",
    "tool_call_id": "call_abc123",
    "arguments": {"path": "src/auth.rs"}
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `tool_name` | string | Tool name |
| `tool_call_id` | string | Unique tool call id |
| `arguments` | object | Tool call arguments |

### `agent.tool.output.delta`

Streaming tool output chunk.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.tool.output.delta",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "delta": "fn login(user: &str)..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `delta` | string | Output text chunk |

### `agent.tool.completed`

Emitted when a tool call finishes.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.tool.completed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "tool_name": "read_file",
    "tool_call_id": "call_abc123",
    "output": "fn login(user: &str) -> Result<Token>...",
    "is_error": false
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `tool_name` | string | Tool name |
| `tool_call_id` | string | Unique tool call id |
| `output` | any | Tool output (string or structured) |
| `is_error` | boolean | Whether the tool returned an error |

### `agent.error`

Emitted when the agent encounters an error.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.error",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "error": { ... }
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `error` | object | AgentError (serialized) |

### `agent.warning`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.warning",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "kind": "token_limit",
    "message": "Approaching context window limit",
    "details": {}
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `kind` | string | Warning kind |
| `message` | string | Warning message |
| `details` | object | Additional details |

### `agent.loop.detected`

Emitted when the agent detects a tool-use loop.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.loop.detected",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {}
}
```

No properties.

### `agent.turn.limit`

Emitted when the agent reaches its maximum turn count.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.turn.limit",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "max_turns": 25
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `max_turns` | number | Maximum turns allowed |

### `agent.skill.expanded`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.skill.expanded",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "skill_name": "read_file"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `skill_name` | string | Expanded skill name |

### `agent.steering.injected`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.steering.injected",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "text": "Remember to run tests after changes"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `text` | string | Injected steering text |

### `agent.compaction.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.compaction.started",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "estimated_tokens": 50000,
    "context_window_size": 128000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `estimated_tokens` | number | Estimated tokens before compaction |
| `context_window_size` | number | Model context window size |

### `agent.compaction.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.compaction.completed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "original_turn_count": 40,
    "preserved_turn_count": 10,
    "summary_token_estimate": 2000,
    "tracked_file_count": 5
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `original_turn_count` | number | Turns before compaction |
| `preserved_turn_count` | number | Turns preserved |
| `summary_token_estimate` | number | Token estimate for summary |
| `tracked_file_count` | number | Files being tracked |

### `agent.llm.retry`

Emitted when an LLM API call is retried.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.llm.retry",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514",
    "attempt": 2,
    "delay_secs": 1.5,
    "error": { ... }
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | LLM provider name |
| `model` | string | Model identifier |
| `attempt` | number | Retry attempt number |
| `delay_secs` | number | Delay before retry in seconds |
| `error` | object | SdkError (serialized) |

### `agent.sub.spawned`

Emitted when a sub-agent is spawned.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.sub.spawned",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "agent_id": "sub_xyz",
    "depth": 1,
    "task": "Write unit tests for auth.rs"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `agent_id` | string | Sub-agent identifier |
| `depth` | number | Nesting depth |
| `task` | string | Task description |

### `agent.sub.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.sub.completed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "agent_id": "sub_xyz",
    "depth": 1,
    "success": true,
    "turns_used": 8
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `agent_id` | string | Sub-agent identifier |
| `depth` | number | Nesting depth |
| `success` | boolean | Whether the sub-agent succeeded |
| `turns_used` | number | Number of turns used |

### `agent.sub.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.sub.failed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "agent_id": "sub_xyz",
    "depth": 1,
    "error": { ... }
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `agent_id` | string | Sub-agent identifier |
| `depth` | number | Nesting depth |
| `error` | object | AgentError (serialized) |

### `agent.sub.closed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.sub.closed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "agent_id": "sub_xyz",
    "depth": 1
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `agent_id` | string | Sub-agent identifier |
| `depth` | number | Nesting depth |

### `agent.mcp.ready`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.mcp.ready",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "server_name": "filesystem",
    "tool_count": 5
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `server_name` | string | MCP server name |
| `tool_count` | number | Number of tools available |

### `agent.mcp.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.mcp.failed",
  "node_id": "code", "node_label": "code",
  "session_id": "ses_abc",
  "properties": {
    "server_name": "filesystem",
    "error": "Connection refused"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `server_name` | string | MCP server name |
| `error` | string | Error message |

### `agent.failover`

Emitted when the agent fails over to a different LLM provider/model.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "agent.failover",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "from_provider": "anthropic",
    "from_model": "claude-sonnet-4-20250514",
    "to_provider": "openai",
    "to_model": "gpt-4o",
    "error": "rate limited"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `from_provider` | string | Original provider |
| `from_model` | string | Original model |
| `to_provider` | string | Failover provider |
| `to_model` | string | Failover model |
| `error` | string | Error that triggered failover |

---

## Subgraph events

### `subgraph.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "subgraph.started",
  "node_id": "pipeline",
  "node_label": "pipeline",
  "properties": {
    "start_node": "sub_start"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `start_node` | string | First node in the subgraph |

### `subgraph.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "subgraph.completed",
  "node_id": "pipeline",
  "node_label": "pipeline",
  "properties": {
    "steps_executed": 4,
    "status": "success",
    "duration_ms": 25000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `steps_executed` | number | Number of steps executed |
| `status` | string | Subgraph outcome status |
| `duration_ms` | number | Subgraph duration |

---

## Sandbox events

Sandbox events have the nested `SandboxEvent` unwrapped into `properties`.

### `sandbox.initializing`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.initializing",
  "properties": {
    "provider": "daytona"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |

### `sandbox.ready`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.ready",
  "properties": {
    "provider": "daytona",
    "duration_ms": 5000,
    "name": "sandbox-01JQXYZ",
    "cpu": 4.0,
    "memory": 8.0,
    "url": "https://sandbox.example.com"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |
| `duration_ms` | number | Initialization duration |
| `name` | string? | Sandbox instance name |
| `cpu` | number? | CPU cores allocated |
| `memory` | number? | Memory in GB allocated |
| `url` | string? | Sandbox URL |

### `sandbox.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.failed",
  "properties": {
    "provider": "daytona",
    "error": "workspace creation failed",
    "duration_ms": 3000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |
| `error` | string | Error message |
| `duration_ms` | number | Time before failure |

### `sandbox.initialized`

Emitted after the engine completes sandbox initialization (distinct from `sandbox.ready` which comes from the sandbox provider).

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.initialized",
  "properties": {
    "working_directory": "/workspace/my-project"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `working_directory` | string | Working directory inside sandbox |

### `sandbox.cleanup.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.cleanup.started",
  "properties": {
    "provider": "daytona"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |

### `sandbox.cleanup.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.cleanup.completed",
  "properties": {
    "provider": "daytona",
    "duration_ms": 2000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |
| `duration_ms` | number | Cleanup duration |

### `sandbox.cleanup.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.cleanup.failed",
  "properties": {
    "provider": "daytona",
    "error": "workspace not found"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `provider` | string | Sandbox provider name |
| `error` | string | Error message |

### `sandbox.snapshot.pulling`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.pulling",
  "properties": {
    "name": "my-image:latest"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Image/snapshot name |

### `sandbox.snapshot.pulled`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.pulled",
  "properties": {
    "name": "my-image:latest",
    "duration_ms": 15000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Image/snapshot name |
| `duration_ms` | number | Pull duration |

### `sandbox.snapshot.ensuring`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.ensuring",
  "properties": {
    "name": "my-snapshot"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Snapshot name |

### `sandbox.snapshot.creating`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.creating",
  "properties": {
    "name": "my-snapshot"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Snapshot name |

### `sandbox.snapshot.ready`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.ready",
  "properties": {
    "name": "my-snapshot",
    "duration_ms": 30000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Snapshot name |
| `duration_ms` | number | Creation duration |

### `sandbox.snapshot.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.snapshot.failed",
  "properties": {
    "name": "my-snapshot",
    "error": "disk quota exceeded"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Snapshot name |
| `error` | string | Error message |

### `sandbox.git.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.git.started",
  "properties": {
    "url": "https://github.com/org/repo.git",
    "branch": "main"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `url` | string | Repository URL |
| `branch` | string? | Branch to clone |

### `sandbox.git.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.git.completed",
  "properties": {
    "url": "https://github.com/org/repo.git",
    "duration_ms": 8000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `url` | string | Repository URL |
| `duration_ms` | number | Clone duration |

### `sandbox.git.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "sandbox.git.failed",
  "properties": {
    "url": "https://github.com/org/repo.git",
    "error": "authentication failed"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `url` | string | Repository URL |
| `error` | string | Error message |

---

## Setup events

### `setup.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "setup.started",
  "properties": {
    "command_count": 3
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `command_count` | number | Number of setup commands |

### `setup.command.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "setup.command.started",
  "properties": {
    "command": "npm install",
    "index": 0
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `command` | string | Command being run |
| `index` | number | Command index |

### `setup.command.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "setup.command.completed",
  "properties": {
    "command": "npm install",
    "index": 0,
    "exit_code": 0,
    "duration_ms": 5000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `command` | string | Command that ran |
| `index` | number | Command index |
| `exit_code` | number | Process exit code |
| `duration_ms` | number | Command duration |

### `setup.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "setup.completed",
  "properties": {
    "duration_ms": 15000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `duration_ms` | number | Total setup duration |

### `setup.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "setup.failed",
  "properties": {
    "command": "npm install",
    "index": 1,
    "exit_code": 1,
    "stderr": "npm ERR! ..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `command` | string | Command that failed |
| `index` | number | Command index |
| `exit_code` | number | Process exit code |
| `stderr` | string | Standard error output |

---

## CLI ensure events

### `cli.ensure.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "cli.ensure.started",
  "properties": {
    "cli_name": "aider",
    "provider": "openai"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `cli_name` | string | CLI tool name |
| `provider` | string | LLM provider |

### `cli.ensure.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "cli.ensure.completed",
  "properties": {
    "cli_name": "aider",
    "provider": "openai",
    "already_installed": true,
    "node_installed": false,
    "duration_ms": 500
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `cli_name` | string | CLI tool name |
| `provider` | string | LLM provider |
| `already_installed` | boolean | Whether it was already present |
| `node_installed` | boolean | Whether Node.js was installed |
| `duration_ms` | number | Duration |

### `cli.ensure.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "cli.ensure.failed",
  "properties": {
    "cli_name": "aider",
    "provider": "openai",
    "error": "pip install failed",
    "duration_ms": 3000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `cli_name` | string | CLI tool name |
| `provider` | string | LLM provider |
| `error` | string | Error message |
| `duration_ms` | number | Duration |

---

## Pull request events

### `pull_request.created`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "pull_request.created",
  "properties": {
    "pr_url": "https://github.com/org/repo/pull/42",
    "pr_number": 42,
    "draft": true
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `pr_url` | string | Pull request URL |
| `pr_number` | number | Pull request number |
| `draft` | boolean | Whether the PR is a draft |

### `pull_request.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "pull_request.failed",
  "properties": {
    "error": "insufficient permissions"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `error` | string | Error message |

---

## Devcontainer events

### `devcontainer.resolved`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.resolved",
  "properties": {
    "dockerfile_lines": 15,
    "environment_count": 3,
    "lifecycle_command_count": 2,
    "workspace_folder": "/workspace"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `dockerfile_lines` | number | Lines in generated Dockerfile |
| `environment_count` | number | Environment variables defined |
| `lifecycle_command_count` | number | Lifecycle commands to run |
| `workspace_folder` | string | Workspace folder path |

### `devcontainer.lifecycle.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.lifecycle.started",
  "properties": {
    "phase": "postCreateCommand",
    "command_count": 2
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `phase` | string | Lifecycle phase name |
| `command_count` | number | Commands in this phase |

### `devcontainer.lifecycle.command.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.lifecycle.command.started",
  "properties": {
    "phase": "postCreateCommand",
    "command": "npm install",
    "index": 0
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `phase` | string | Lifecycle phase name |
| `command` | string | Command being run |
| `index` | number | Command index |

### `devcontainer.lifecycle.command.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.lifecycle.command.completed",
  "properties": {
    "phase": "postCreateCommand",
    "command": "npm install",
    "index": 0,
    "exit_code": 0,
    "duration_ms": 8000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `phase` | string | Lifecycle phase name |
| `command` | string | Command that ran |
| `index` | number | Command index |
| `exit_code` | number | Process exit code |
| `duration_ms` | number | Command duration |

### `devcontainer.lifecycle.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.lifecycle.completed",
  "properties": {
    "phase": "postCreateCommand",
    "duration_ms": 12000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `phase` | string | Lifecycle phase name |
| `duration_ms` | number | Phase duration |

### `devcontainer.lifecycle.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "devcontainer.lifecycle.failed",
  "properties": {
    "phase": "postCreateCommand",
    "command": "npm install",
    "index": 0,
    "exit_code": 1,
    "stderr": "npm ERR! ..."
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `phase` | string | Lifecycle phase name |
| `command` | string | Command that failed |
| `index` | number | Command index |
| `exit_code` | number | Process exit code |
| `stderr` | string | Standard error output |

---

## Asset events

### `asset.captured`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "asset.captured",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "attempt": 1,
    "node_slug": "code",
    "path": "screenshot.png",
    "mime": "image/png",
    "content_md5": "d41d8cd98f00b204e9800998ecf8427e",
    "content_sha256": "e3b0c44298fc1c149afbf4c8996fb924...",
    "bytes": 45000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `attempt` | number | Attempt number |
| `node_slug` | string | Node slug for asset path |
| `path` | string | Asset file path |
| `mime` | string | MIME type |
| `content_md5` | string | MD5 hash |
| `content_sha256` | string | SHA-256 hash |
| `bytes` | number | File size in bytes |

---

## SSH events

### `ssh.ready`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "ssh.ready",
  "properties": {
    "ssh_command": "ssh user@host -p 2222"
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `ssh_command` | string | SSH command to connect |

---

## Watchdog events

### `watchdog.timeout`

Emitted when the stall watchdog detects no progress.

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "watchdog.timeout",
  "node_id": "code",
  "node_label": "code",
  "properties": {
    "idle_seconds": 1800
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `idle_seconds` | number | Seconds since last activity |

---

## Retro events

### `retro.started`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "retro.started",
  "properties": {}
}
```

No properties.

### `retro.completed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "retro.completed",
  "properties": {
    "duration_ms": 5000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `duration_ms` | number | Retro duration |

### `retro.failed`

```json
{
  "id": "...", "ts": "...", "run_id": "...",
  "event": "retro.failed",
  "properties": {
    "error": "LLM request failed",
    "duration_ms": 3000
  }
}
```

| Property | Type | Description |
|----------|------|-------------|
| `error` | string | Error message |
| `duration_ms` | number | Duration before failure |
