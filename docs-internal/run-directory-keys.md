# Run Directory Keys

All keys that may be written to the run store during a workflow execution.

## 1. `_init.json`

Store initialization metadata. Written when the store is created.

- `run_id` — ULID string
- `created_at` — RFC 3339 timestamp
- `db_prefix` — SlateDB key prefix
- `run_dir` — path to run directory (optional)

## 2. `run.json`

Run configuration snapshot. Written at run creation.

- `run_id` — ULID string
- `created_at` — RFC 3339 timestamp
- `settings` — full `FabroSettings` object (see `workflow.toml` fields)
- `graph` — parsed workflow graph
  - `name` — graph name
  - `nodes` — map of node id to node object
    - `id` — node identifier
    - `attrs` — map of attribute name to typed value (`String`, `Integer`, `Float`, `Boolean`, `Duration`)
    - `classes` — list of CSS-like class names (optional)
  - `edges` — list of edge objects
    - `from` — source node id
    - `to` — target node id
    - `attrs` — map of attribute name to typed value
  - `attrs` — graph-level attributes map
- `workflow_slug` — workflow slug (optional)
- `working_directory` — path string
- `host_repo_path` — original host repo path (optional)
- `base_branch` — base git branch name (optional)
- `labels` — string key-value map (optional)

## 3. `start.json`

Start timestamp and git context. Written when execution begins.

- `run_id` — ULID string
- `start_time` — RFC 3339 timestamp
- `run_branch` — git branch created for the run (optional)
- `base_sha` — base commit SHA (optional)

## 4. `checkpoint.json`

Latest execution state. Updated after each node completes.

- `timestamp` — RFC 3339 timestamp
- `current_node` — id of the node being executed
- `completed_nodes` — list of completed node ids
- `node_retries` — map of node id to retry count
- `context_values` — map of context key to arbitrary JSON value
- `node_outcomes` — map of node id to outcome object (optional)
  - `status` — `"success"` | `"fail"` | `"skipped"` | `"partial_success"` | `"retry"`
  - `preferred_label` — edge label hint for routing (optional)
  - `suggested_next_ids` — list of suggested successor node ids (optional)
  - `context_updates` — map of context key to JSON value (optional)
  - `jump_to_node` — target node for non-edge jump (optional)
  - `notes` — free-text notes (optional)
  - `failure` — failure detail object (optional)
    - `message` — error description
    - `failure_class` — `"transient_infra"` | `"deterministic"` | `"budget_exhausted"` | `"compilation_loop"` | `"canceled"` | `"structural"`
    - `failure_signature` — dedup key for repeated failures (optional)
  - `usage` — token usage object (optional, null when absent)
    - `model` — model identifier
    - `input_tokens` — input token count
    - `output_tokens` — output token count
    - `cache_read_tokens` — cache read tokens (optional)
    - `cache_write_tokens` — cache write tokens (optional)
    - `reasoning_tokens` — reasoning/thinking tokens (optional)
    - `speed` — speed tier (optional)
    - `cost` — estimated cost in USD (optional)
  - `files_touched` — list of file paths modified (optional)
  - `duration_ms` — stage duration in milliseconds (optional)
- `next_node_id` — pre-selected next node (optional)
- `git_commit_sha` — current HEAD SHA (optional)
- `loop_failure_signatures` — map of failure signature to count (optional)
- `restart_failure_signatures` — map of failure signature to count (optional)
- `node_visits` — map of node id to visit count (optional)

## 5. `conclusion.json`

Final run summary. Written when the run finishes.

- `timestamp` — RFC 3339 timestamp
- `status` — `"success"` | `"fail"` | `"skipped"` | `"partial_success"` | `"retry"`
- `duration_ms` — total run duration in milliseconds
- `failure_reason` — error message (optional)
- `final_git_commit_sha` — final HEAD SHA (optional)
- `stages` — list of stage summary objects (optional)
  - `stage_id` — node id
  - `stage_label` — display label
  - `duration_ms` — stage duration
  - `cost` — estimated cost in USD (optional)
  - `retries` — retry count
- `total_cost` — aggregate cost in USD (optional)
- `total_retries` — aggregate retry count
- `total_input_tokens` — aggregate input tokens
- `total_output_tokens` — aggregate output tokens
- `total_cache_read_tokens` — aggregate cache read tokens
- `total_cache_write_tokens` — aggregate cache write tokens
- `total_reasoning_tokens` — aggregate reasoning tokens
- `has_pricing` — whether cost data is available

## 6. `retro.json`

Retrospective analysis. Written after the retro agent completes.

- `run_id` — ULID string
- `workflow_name` — workflow name
- `goal` — workflow goal text
- `timestamp` — RFC 3339 timestamp
- `smoothness` — `"effortless"` | `"smooth"` | `"bumpy"` | `"struggled"` | `"failed"` (optional)
- `stages` — list of stage retro objects
  - `stage_id` — node id
  - `stage_label` — display label
  - `status` — status string
  - `duration_ms` — stage duration
  - `retries` — retry count
  - `cost` — estimated cost in USD (optional)
  - `notes` — free-text notes (optional)
  - `failure_reason` — error message (optional)
  - `files_touched` — list of file paths (optional)
- `stats` — aggregate stats object
  - `total_duration_ms` — total duration
  - `total_cost` — aggregate cost (optional)
  - `total_retries` — aggregate retries
  - `files_touched` — deduplicated file list (optional)
  - `stages_completed` — count of completed stages
  - `stages_failed` — count of failed stages
- `intent` — what the run intended to do (optional)
- `outcome` — what actually happened (optional)
- `learnings` — list of learning objects (optional)
  - `category` — `"repo"` | `"code"` | `"workflow"` | `"tool"`
  - `text` — learning description
- `friction_points` — list of friction point objects (optional)
  - `kind` — `"retry"` | `"timeout"` | `"wrong_approach"` | `"tool_failure"` | `"ambiguity"`
  - `description` — friction description
  - `stage_id` — related node id (optional)
- `open_items` — list of open item objects (optional)
  - `kind` — `"tech_debt"` | `"follow_up"` | `"investigation"` | `"test_gap"`
  - `description` — item description

## 7. `sandbox.json`

Sandbox environment details. Written when the sandbox is ready.

- `provider` — sandbox provider name (e.g. `"local"`, `"docker"`, `"daytona"`)
- `working_directory` — working directory inside the sandbox
- `identifier` — sandbox instance identifier (optional)
- `host_working_directory` — host-side working directory (optional)
- `container_mount_point` — mount point inside the container (optional)

## 8. `workflow.fabro`

Raw Graphviz dot source for the workflow graph. Plain text, not JSON.

## 9. `workflow.toml`

Workflow configuration in TOML format. Same schema as `settings` in `run.json`.

## 10. `checkpoints/{seq:04}-{epoch_ms}.json`

Checkpoint history snapshots. Same schema as `checkpoint.json` (#4). Filename is a zero-padded sequence number followed by epoch milliseconds (e.g. `0042-1706234567890.json`).

## 11. `nodes/{node_id}/prompt.md`

Prompt sent to the LLM for agent or prompt nodes. Plain text/markdown, not JSON.

## 12. `nodes/{node_id}/response.md`

Response received from the LLM. Plain text/markdown, not JSON.

## 13. `nodes/{node_id}/status.json`

Node execution status. Written when a node completes.

- `status` — `"success"` | `"fail"` | `"skipped"` | `"partial_success"` | `"retry"`
- `notes` — free-text notes (optional)
- `failure_reason` — error message (optional)
- `timestamp` — RFC 3339 timestamp

## 14. `nodes/{node_id}/stdout.log`

Standard output from command nodes. Plain text, not JSON.

## 15. `nodes/{node_id}/stderr.log`

Standard error from command nodes. Plain text, not JSON.

## 16. `nodes/{node_id}/cli_stdout.log`

Standard output from CLI-backend LLM invocations. Plain text, not JSON.

## 17. `nodes/{node_id}/cli_stderr.log`

Standard error from CLI-backend LLM invocations. Plain text, not JSON.

## 18. `nodes/{node_id}/diff.patch`

Git diff of sandbox changes made by the node. Plain text unified diff, not JSON.

## 19. `nodes/{node_id}/provider_used.json`

LLM provider metadata. Written for agent, prompt, and CLI-backend nodes.

- `mode` — `"agent"` | `"prompt"` | `"cli"`
- `provider` — provider name (e.g. `"anthropic"`, `"openai"`, `"gemini"`)
- `model` — model identifier
- `command` — CLI command string (only present when `mode` is `"cli"`)

## 20. `nodes/{node_id}/script_invocation.json`

Command node invocation details. Written before the command runs.

- `command` — shell command or script body
- `language` — `"shell"` | `"python"`
- `timeout_ms` — timeout in milliseconds (null if no timeout)

## 21. `nodes/{node_id}/script_timing.json`

Command node execution timing. Written after the command completes.

- `duration_ms` — execution duration in milliseconds
- `exit_code` — process exit code (null if timed out)
- `timed_out` — whether the command was killed by timeout

## 22. `nodes/{node_id}/parallel_results.json`

Results from parallel branch execution. Written by the parallel handler.

Array of objects, each with:
- `id` — branch node id
- `status` — status string (e.g. `"success"`, `"fail"`)
- `head_sha` — git HEAD SHA from the branch worktree (optional, present only if set)

## 23. `retro/prompt.md`

Prompt sent to the retro agent. Plain text/markdown, not JSON.

## 24. `retro/response.md`

Response received from the retro agent. Plain text/markdown, not JSON.

## 25. `retro/status.json`

Retro agent execution status.

- `outcome` — `"success"` | `"failure"`
- `failure_reason` — error message (null on success)
- `timestamp` — RFC 3339 timestamp

## 26. `retro/provider_used.json`

Retro agent LLM provider metadata.

- `mode` — always `"agent"`
- `provider` — provider name (e.g. `"anthropic"`, `"openai"`)
- `model` — model identifier

---

**Node visit directories:** The first visit writes to `nodes/{node_id}/`. Subsequent visits write to `nodes/{node_id}-visit_{N}/` where N is the visit number.
