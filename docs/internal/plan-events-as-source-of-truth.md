# Plan: Events as Source of Truth

Make all run directory key data derivable from events in `progress.jsonl`.

## Summary of changes

### Clarified derivation rules

- Envelope fields count as event sources. Any file field may be sourced from event envelope metadata (`run_id`, `ts`, `node_id`, `node_label`) as well as `properties`.
- When a file is reconstructed from multiple events, the derivation should be documented explicitly in `events.md` and `run-directory-keys.md`; consumers should not need to infer cross-event joins.
- `retro.json` is reconstructed from multiple events: `run.started` provides `workflow_name` and `goal`, `retro.completed` provides the parsed `retro` payload, and the retro event envelope provides the completion timestamp.
- `conclusion.json.final_git_commit_sha` is normalized from the terminal run event: `run.completed.properties.final_git_commit_sha` on success/partial success, `run.failed.properties.git_commit_sha` on failure.

### New events

| Event | Purpose |
|-------|---------|
| `run.created` | Full run definition (settings, graph, workflow source/config, labels, paths). Emitted at end of CREATE operation, before START. |
| `command.started` | Command node invocation: script, language, timeout_ms |
| `command.completed` | Command node result: stdout, stderr, exit_code, duration_ms, timed_out |
| `agent.cli.started` | CLI-backend LLM invocation: mode, provider, model, command |
| `agent.cli.completed` | CLI-backend LLM result: stdout, stderr |

### Enriched events

| Event | New fields |
|-------|------------|
| `stage.started` | Remove `script` (moved to `command.started`), make `handler_type` non-optional |
| `stage.completed` | `context_updates`, `jump_to_node`, `context_values`, `node_visits`, `loop_failure_signatures`, `restart_failure_signatures`, `response` |
| `sandbox.initialized` | `provider`, `identifier`, `working_directory`, `repo_cloned`, `clone_origin_url`, `clone_branch` |
| `checkpoint.completed` | `diff` |
| `parallel.branch.completed` | `head_sha` |
| `agent.session.started` | `mode`, `provider`, `model` |
| `stage.prompt` | `mode`, `provider`, `model` |
| `retro.started` | `prompt`, `provider`, `model` |
| `retro.completed` | `response`, `retro` (full Retro struct) |

### Already covered (no changes needed)

| Key | Event source |
|-----|--------------|
| `start.json` | `run.started` |
| `nodes/{node_id}/prompt.md` | `stage.prompt` |
| `nodes/{node_id}/status.json` | `stage.completed` |
| `retro/status.json` | `retro.completed` / `retro.failed` |
| `conclusion.json` | `run.completed` / `run.failed` + aggregation from `stage.completed` |
| `checkpoints/*.json` | Same as `checkpoint.json` — derived from `stage.completed` replay |

---

## Detailed decisions

### 1–2. `_init.json` + `run.json` — new `run.created` event

**Decision:** Emit a `run.created` event at the end of the CREATE operation (before START). Carries everything needed to persist the run:

- From `_init.json`: `created_at`, `db_prefix`, `run_dir`
- From `run.json`: `settings`, `graph`, `workflow_slug`, `source_directory`, `base_branch`, `labels`
- From `workflow.fabro`/`workflow.toml`: `workflow_source` (raw dot text), `workflow_config` (raw TOML text)

The existing `run.started` stays lightweight — it signals execution has begun. The CREATE→START boundary is: `run.created` persists the run definition, `run.started` marks execution start.

Derivation details:
- `_init.json.run_id` and `run.json.run_id` come from `run.created` envelope `run_id`
- `_init.json.created_at` and `run.json.created_at` come from `run.created` envelope `ts`

### 3. `checkpoint.json` — internal engine state

**Decision:** Add `context_updates`, `jump_to_node`, `context_values`, `node_visits`, `loop_failure_signatures`, and `restart_failure_signatures` to the `stage.completed` event properties. All checkpoint fields become derivable from replaying stage events.

New fields on `stage.completed`:
- `context_updates` — map of context key → JSON value set by this stage
- `jump_to_node` — non-edge jump target (optional)
- `context_values` — full accumulated context map after this stage
- `node_visits` — map of node id → visit count after this stage
- `loop_failure_signatures` — failure signature → count after this stage
- `restart_failure_signatures` — failure signature → count after this stage

Derivation details:
- `checkpoint.json.timestamp` comes from the corresponding `checkpoint.completed` envelope `ts`
- `checkpoints/{seq:04}-{epoch_ms}.json` uses the same derivation as `checkpoint.json`, with snapshot timestamp taken from each `checkpoint.completed` envelope `ts`

### 4. `retro.json` — LLM-generated retro content

**Decision:** Embed the full `Retro` struct as a `retro` property on the `retro.completed` event.

Derivation details:
- `retro.json.run_id` comes from the `retro.completed` envelope `run_id`
- `retro.json.workflow_name` comes from `run.started.properties.name`
- `retro.json.goal` comes from `run.started.properties.goal`
- `retro.json.timestamp` comes from the `retro.completed` envelope `ts`
- The remaining retro content comes from `retro.completed.properties.retro`

### 5. `sandbox.json` — missing fields

**Decision:** Add sandbox-native identity and clone metadata to `sandbox.initialized` so it alone is sufficient to reconstruct `sandbox.json`. Host paths and container mount points are not part of the durable event model; clone-based providers report their sandbox working directory plus repository clone state instead.

### 6. `workflow.fabro` + `workflow.toml` — raw source files

**Decision:** Add `workflow_source` (raw dot text) and `workflow_config` (raw TOML text) as string fields on `run.created`. Covered by gap 1–2.

### 7. `nodes/{node_id}/response.md` — LLM response text

**Decision:** Add a `response` string field to `stage.completed` for LLM stages.

### 8. `nodes/{node_id}/prompt.md` — already covered

No change needed. `stage.prompt` is emitted per handler invocation (including retries) with the full rendered prompt text.

### 9–10. `stdout.log`, `stderr.log`, `script_timing.json` — new `command.completed` event

**Decision:** Emit a `command.completed` event after a command node finishes. Fields:
- `stdout`, `stderr`, `exit_code`, `duration_ms`, `timed_out`

The `node_id` is in the envelope.

### 11. `nodes/{node_id}/script_invocation.json` — new `command.started` event

**Decision:** Emit a `command.started` event with `script`, `language`, `timeout_ms`. Pairs with `command.completed`. Remove `script` from `stage.started` — it's handler-specific data that belongs on the handler-specific event (same pattern as `agent.session.started` and `stage.prompt`).

### 12. `cli_stdout.log`, `cli_stderr.log`, `provider_used.json` (CLI) — new `agent.cli.started` / `agent.cli.completed` events

**Decision:** Emit an `agent.cli.started` event before the CLI subprocess starts, with `mode` ("cli"), `provider`, `model`, `command`. Emit `agent.cli.completed` after it finishes, with `stdout`, `stderr`. The `node_id` is in the envelope.

### 13. `nodes/{node_id}/diff.patch` — git diff

**Decision:** Add a `diff` string field to `checkpoint.completed`.

### 14. `nodes/{node_id}/provider_used.json` — provider metadata

**Decision:** Add provider metadata to handler-specific events:
- `agent.session.started` gets `mode` ("agent"), `provider`, `model`
- `agent.cli.started` gets `mode` ("cli"), `provider`, `model`, `command`
- `stage.prompt` gets `mode` ("prompt"), `provider`, `model`

### 15. `parallel_results.json` — `head_sha`

**Decision:** Add an optional `head_sha` field to `parallel.branch.completed`.

### 16. `retro/prompt.md` + `retro/response.md`

**Decision:** Add `prompt` to `retro.started`. Add `response` to `retro.completed` (alongside the parsed retro struct).

### 17. `retro/provider_used.json`

**Decision:** Add `provider` and `model` to `retro.started` alongside the prompt.

### Conclusion timestamps and final SHA normalization

**Decision:** Treat terminal run events as the authoritative source for `conclusion.json`.

Derivation details:
- `conclusion.json.timestamp` comes from the terminal event envelope `ts` (`run.completed` or `run.failed`)
- `conclusion.json.final_git_commit_sha` comes from `run.completed.properties.final_git_commit_sha` on success/partial success
- On failure, `conclusion.json.final_git_commit_sha` is reconstructed from `run.failed.properties.git_commit_sha`

---

## Follow-up improvements

- `node_visits`, `loop_failure_signatures`, `restart_failure_signatures` on `stage.completed` are snapshots of accumulated state. In the future, these could be derived from event replay instead of being carried on each event — removing them would reduce event size but require replay logic in every consumer.
- `context_values` is the full accumulated context map. If context maps grow large, a future optimization could emit only `context_updates` (the delta) and require consumers to accumulate. For now, shipping the full snapshot is simpler.
- `stdout`/`stderr` on `command.completed` could be large. If this becomes a problem, consider a size threshold with truncation or a separate blob store with a reference in the event.
