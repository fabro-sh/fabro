# Run Directory Keys

All keys that may be written to the run store during a workflow execution, with event source mappings.

## 1. `_init.json`

Store initialization metadata. Written when the store is created.

No event source — written directly at store creation time.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `run_id` | ULID string | — |
| `created_at` | RFC 3339 timestamp | — |
| `db_prefix` | SlateDB key prefix | — |
| `run_dir` | path to run directory (optional) | — |

## 2. `run.json`

Run configuration snapshot. Written at run creation.

No single event carries this data. The `run.started` event has a subset (`name`, `run_id`, `base_branch`, `base_sha`, `run_branch`, `goal`) but not `settings`, `graph`, or `labels`.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `run_id` | ULID string | `run.started` → `envelope.run_id` |
| `created_at` | RFC 3339 timestamp | — |
| `settings` | full Settings object | — |
| `graph` | parsed workflow graph | — |
| `workflow_slug` | workflow slug (optional) | — |
| `working_directory` | path string | — |
| `host_repo_path` | original host repo path (optional) | — |
| `base_branch` | base git branch (optional) | `run.started` → `properties.base_branch` |
| `labels` | string key-value map (optional) | — |

## 3. `start.json`

Start timestamp and git context. Written when execution begins.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `run_id` | ULID string | `run.started` → `envelope.run_id` |
| `start_time` | RFC 3339 timestamp | `run.started` → `envelope.ts` |
| `run_branch` | git branch for the run (optional) | `run.started` → `properties.run_branch` |
| `base_sha` | base commit SHA (optional) | `run.started` → `properties.base_sha` |

## 4. `checkpoint.json`

Latest execution state. Updated after each node completes.

Checkpoint is an accumulated snapshot built from multiple events over time. Individual fields map to specific events, but the full object is never in a single event.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `timestamp` | RFC 3339 timestamp | — (written at checkpoint time) |
| `current_node` | node being executed | `stage.started` → `envelope.node_id` |
| `completed_nodes` | list of completed node ids | accumulated from `stage.completed` → `envelope.node_id` |
| `node_retries` | map of node id → retry count | accumulated from `stage.retrying` → `envelope.node_id` + `properties.attempt` |
| `context_values` | map of context key → JSON value | — (internal engine state) |
| `node_outcomes` | map of node id → outcome | see below |
| `next_node_id` | pre-selected next node (optional) | `edge.selected` → `properties.to_node` |
| `git_commit_sha` | current HEAD SHA (optional) | `checkpoint.completed` → `properties.git_commit_sha` |
| `loop_failure_signatures` | failure signature → count (optional) | — (internal engine state) |
| `restart_failure_signatures` | failure signature → count (optional) | — (internal engine state) |
| `node_visits` | node id → visit count (optional) | — (internal engine state) |

**`node_outcomes[node_id]`** — each outcome maps to `stage.completed`:

| Field | Description | Event Source |
|-------|-------------|--------------|
| `status` | `"success"` / `"fail"` / `"skipped"` / `"partial_success"` / `"retry"` | `stage.completed` → `properties.status` |
| `preferred_label` | edge label hint (optional) | `stage.completed` → `properties.preferred_label` |
| `suggested_next_ids` | successor node ids (optional) | `stage.completed` → `properties.suggested_next_ids` |
| `context_updates` | context key → JSON value (optional) | — (not in event) |
| `jump_to_node` | non-edge jump target (optional) | — (not in event) |
| `notes` | free-text notes (optional) | `stage.completed` → `properties.notes` |
| `failure.message` | error description | `stage.completed` → `properties.error` (flattened) |
| `failure.failure_class` | failure category | `stage.completed` → `properties.failure_class` (flattened) |
| `failure.failure_signature` | dedup key (optional) | `stage.completed` → `properties.failure_signature` (flattened) |
| `usage.model` | model identifier | `stage.completed` → `properties.usage.model` |
| `usage.input_tokens` | input token count | `stage.completed` → `properties.usage.input_tokens` |
| `usage.output_tokens` | output token count | `stage.completed` → `properties.usage.output_tokens` |
| `usage.cache_read_tokens` | cache read tokens (optional) | `stage.completed` → `properties.usage.cache_read_tokens` |
| `usage.cache_write_tokens` | cache write tokens (optional) | `stage.completed` → `properties.usage.cache_write_tokens` |
| `usage.reasoning_tokens` | reasoning tokens (optional) | `stage.completed` → `properties.usage.reasoning_tokens` |
| `usage.speed` | speed tier (optional) | `stage.completed` → `properties.usage.speed` |
| `usage.cost` | estimated cost in USD (optional) | `stage.completed` → `properties.usage.cost` |
| `files_touched` | file paths modified (optional) | `stage.completed` → `properties.files_touched` |
| `duration_ms` | stage duration (optional) | `stage.completed` → `properties.duration_ms` |

## 5. `conclusion.json`

Final run summary. Written when the run finishes.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `timestamp` | RFC 3339 timestamp | — (written at conclusion time) |
| `status` | final status | `run.completed` → `properties.status` |
| `duration_ms` | total run duration | `run.completed` → `properties.duration_ms` |
| `failure_reason` | error message (optional) | `run.failed` → `properties.error` |
| `final_git_commit_sha` | final HEAD SHA (optional) | `run.completed` → `properties.final_git_commit_sha` |
| `stages` | list of stage summaries (optional) | — (aggregated, not in events) |
| `stages[].stage_id` | node id | `stage.completed` → `envelope.node_id` |
| `stages[].stage_label` | display label | `stage.completed` → `envelope.node_label` |
| `stages[].duration_ms` | stage duration | `stage.completed` → `properties.duration_ms` |
| `stages[].cost` | cost in USD (optional) | `stage.completed` → `properties.usage.cost` |
| `stages[].retries` | retry count | accumulated from `stage.retrying` events |
| `total_cost` | aggregate cost (optional) | `run.completed` → `properties.total_cost` |
| `total_retries` | aggregate retries | — (aggregated from stage events) |
| `total_input_tokens` | aggregate input tokens | `run.completed` → `properties.usage.input_tokens` |
| `total_output_tokens` | aggregate output tokens | `run.completed` → `properties.usage.output_tokens` |
| `total_cache_read_tokens` | aggregate cache read tokens | `run.completed` → `properties.usage.cache_read_tokens` |
| `total_cache_write_tokens` | aggregate cache write tokens | `run.completed` → `properties.usage.cache_write_tokens` |
| `total_reasoning_tokens` | aggregate reasoning tokens | `run.completed` → `properties.usage.reasoning_tokens` |
| `has_pricing` | whether cost data is available | — (derived from `total_cost`) |

## 6. `retro.json`

Retrospective analysis. Written after the retro agent completes.

No direct event mapping — this is generated by the retro agent's LLM response. The `retro.completed` event only carries `duration_ms`.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `run_id` | ULID string | — |
| `workflow_name` | workflow name | — |
| `goal` | workflow goal text | — |
| `timestamp` | RFC 3339 timestamp | — |
| `smoothness` | rating (optional) | — (LLM-generated) |
| `stages` | list of stage retro objects | — (LLM-generated) |
| `stats` | aggregate stats object | — (computed from stage data) |
| `intent` | what the run intended to do (optional) | — (LLM-generated) |
| `outcome` | what actually happened (optional) | — (LLM-generated) |
| `learnings` | list of learnings (optional) | — (LLM-generated) |
| `friction_points` | list of friction points (optional) | — (LLM-generated) |
| `open_items` | list of open items (optional) | — (LLM-generated) |

## 7. `sandbox.json`

Sandbox environment details. Written when the sandbox is ready.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `provider` | provider name | `sandbox.ready` → `properties.provider` |
| `working_directory` | working directory in sandbox | `sandbox.initialized` → `properties.working_directory` |
| `identifier` | instance identifier (optional) | `sandbox.ready` → `properties.name` |
| `host_working_directory` | host-side path (optional) | — |
| `container_mount_point` | container mount point (optional) | — |

## 8. `workflow.fabro`

Raw Graphviz dot source for the workflow graph. Plain text, not JSON.

No event source — written directly from the parsed graph.

## 9. `workflow.toml`

Workflow configuration in TOML format. Same schema as `settings` in `run.json`.

No event source — copied from the workflow definition.

## 10. `checkpoints/{seq:04}-{epoch_ms}.json`

Checkpoint history snapshots. Same schema as `checkpoint.json` (#4).

Each snapshot is written on `checkpoint.completed` events.

## 11. `nodes/{node_id}/prompt.md`

Prompt sent to the LLM for agent or prompt nodes. Plain text/markdown, not JSON.

Partial event source: `stage.prompt` → `properties.text` carries the rendered prompt text.

## 12. `nodes/{node_id}/response.md`

Response received from the LLM. Plain text/markdown, not JSON.

Reconstructable from `agent.message` → `properties.text` events (one per LLM turn), but the file contains only the final response.

## 13. `nodes/{node_id}/status.json`

Node execution status. Written when a node completes.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `status` | stage status | `stage.completed` → `properties.status` |
| `notes` | free-text notes (optional) | `stage.completed` → `properties.notes` |
| `failure_reason` | error message (optional) | `stage.completed` → `properties.error` (flattened from failure) |
| `timestamp` | RFC 3339 timestamp | `stage.completed` → `envelope.ts` |

## 14. `nodes/{node_id}/stdout.log`

Standard output from command nodes. Plain text, not JSON.

No event source — captured from sandbox exec, not emitted as events.

## 15. `nodes/{node_id}/stderr.log`

Standard error from command nodes. Plain text, not JSON.

No event source — captured from sandbox exec, not emitted as events.

## 16. `nodes/{node_id}/cli_stdout.log`

Standard output from CLI-backend LLM invocations. Plain text, not JSON.

No event source — captured from CLI subprocess, not emitted as events.

## 17. `nodes/{node_id}/cli_stderr.log`

Standard error from CLI-backend LLM invocations. Plain text, not JSON.

No event source — captured from CLI subprocess, not emitted as events.

## 18. `nodes/{node_id}/diff.patch`

Git diff of sandbox changes made by the node. Plain text unified diff, not JSON.

No event source — generated from git at checkpoint time.

## 19. `nodes/{node_id}/provider_used.json`

LLM provider metadata. Written for agent, prompt, and CLI-backend nodes.

No direct event. Closest: `agent.failover` carries `from_provider`/`to_provider`/`from_model`/`to_model`, but only on failover. The initial provider choice is not emitted as an event.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `mode` | `"agent"` / `"prompt"` / `"cli"` | — |
| `provider` | provider name | — |
| `model` | model identifier | `stage.completed` → `properties.usage.model` (indirect) |
| `command` | CLI command (only when mode=cli) | — |

## 20. `nodes/{node_id}/script_invocation.json`

Command node invocation details. Written before the command runs.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `command` | shell command or script body | `stage.started` → `properties.script` (when handler_type is command) |
| `language` | `"shell"` / `"python"` | — |
| `timeout_ms` | timeout in milliseconds (null if none) | — |

## 21. `nodes/{node_id}/script_timing.json`

Command node execution timing. Written after the command completes.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `duration_ms` | execution duration | `stage.completed` → `properties.duration_ms` |
| `exit_code` | process exit code (null if timed out) | — |
| `timed_out` | whether command was killed by timeout | — |

## 22. `nodes/{node_id}/parallel_results.json`

Results from parallel branch execution. Written by the parallel handler.

Array of objects:

| Field | Description | Event Source |
|-------|-------------|--------------|
| `id` | branch node id | `parallel.branch.completed` → `envelope.node_id` |
| `status` | status string | `parallel.branch.completed` → `properties.status` |
| `head_sha` | git HEAD SHA (optional) | — |

## 23. `retro/prompt.md`

Prompt sent to the retro agent. Plain text/markdown, not JSON.

No event source.

## 24. `retro/response.md`

Response received from the retro agent. Plain text/markdown, not JSON.

No event source.

## 25. `retro/status.json`

Retro agent execution status.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `outcome` | `"success"` / `"failure"` | `retro.completed` or `retro.failed` (inferred from which event fires) |
| `failure_reason` | error message (null on success) | `retro.failed` → `properties.error` |
| `timestamp` | RFC 3339 timestamp | `retro.completed` → `envelope.ts` or `retro.failed` → `envelope.ts` |

## 26. `retro/provider_used.json`

Retro agent LLM provider metadata.

No event source — written directly by the retro agent.

| Field | Description | Event Source |
|-------|-------------|--------------|
| `mode` | always `"agent"` | — |
| `provider` | provider name | — |
| `model` | model identifier | — |

---

**Node visit directories:** The first visit writes to `nodes/{node_id}/`. Subsequent visits write to `nodes/{node_id}-visit_{N}/` where N is the visit number.
