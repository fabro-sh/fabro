All 4 server-side tests pass:
- `list_run_stages_distinguishes_visits` (new) — multi-visit verification
- `list_run_stages_shows_retrying_after_failed_event` (new) — failed→retrying = Retrying
- `list_run_stages_shows_retrying_when_failed_will_retry` (new) — will_retry=true alone = Retrying
- `list_run_stages_projects_retrying_until_completion` (existing, updated)

Let me do a final summary of what was done:

## Summary

I implemented every step of the plan with TDD red/green:

### OpenAPI (step 1)
- `RunStage`: required `id` (now `node_id@visit` form), required `node_id` (renamed from `dot_id`), required `visit: NonZeroU32` (`format: uint32`, `minimum: 1`).

### Generated code (step 2)
- `cargo build -p fabro-api` regenerated Rust types (`pub node_id: String`, `pub visit: NonZeroU32`).
- TS client regenerated (`run-stage.ts` updated; once Java was available the regen output exactly matched my hand-edit).

### Workflow lib (step 3)
- Added `extract_stage_durations_by_stage_id(events) -> HashMap<StageId, u64>` alongside the existing per-node helper.

### Server handler (step 4)
- Rewrote `list_run_stages`: builds projection via `RunProjection::apply_events(events)`, iterates `iter_stages()` sorted by `first_event_seq`, derives status from the latest lifecycle event for each `stage_id` (with `will_retry` honored on `StageFailed`), uses `extract_stage_durations_by_stage_id`. Removed the `next_node_id` branch and the `checkpoint.completed_nodes` walk.
- Replaced `active_stage_state_from_events` with `stage_status_from_events` that scans `stage.started`/`stage.retrying`/`stage.completed`/`stage.failed` and falls back to projection completion only when no lifecycle events exist.

### Demo fixtures (step 5)
- 4 single-visit entries get `@1` suffix; first 4 marked `Succeeded`, fifth `apply-changes@2` marked `Running` to demo the multi-visit path.

### Frontend (steps 6–10)
- `stage-sidebar.ts`: pass through `visit`/`nodeId`, filter by `node_id`, dropped `?? stage.id` fallback.
- `stage-sidebar.tsx`: `Stage` interface — `nodeId: string`, `visit: number`; render `${name} (${visit})` when `visit > 1`.
- `run-events.ts`: added `stage_id?: string` to payload; `stageIdFromPayload` prefers `stage_id`; added `stage.retrying` to `STAGE_EVENTS`.
- `run-stages.tsx`: filter events by `e.stage_id === stageId`; dropped `@1` fallbacks (use `stageId` parameter); header renders visit suffix when `visit > 1`.
- `run-overview.tsx`: replaced 3 separate dot-id sets with `aggregateGraphNodeStatus()` (latest active visit wins; otherwise latest visit's terminal state; click target = latest visit).

### Tests (step 11)
- Server (`tests.rs`): added `list_run_stages_distinguishes_visits`, `list_run_stages_shows_retrying_after_failed_event`, `list_run_stages_shows_retrying_when_failed_will_retry`. Updated existing test to emit scoped events. Added `append_scoped_stage_event` helper.
- Web tests:
  - `lib/run-events.test.tsx`: `queryKeysForRunEvent` covers `stage.retrying`; SSE envelope with `stage_id: verify@2` invalidates `stageTurns(runId, "verify@2")` and not `verify`.
  - `lib/stage-sidebar.test.ts` (new): map two visits → distinct entries; visibility filter on `node_id`; `aggregateGraphNodeStatus` covers all four cascade cases.
  - `routes/run-stages.test.ts`: `turnsFromEvents` filters on `stage_id` (no cross-contamination between `verify@1`/`verify@2`); command turn carries the requested `stageId` without `@1` fallback.

### Verification
- `cargo build --workspace` ✅
- `cargo nextest run --workspace` ✅ (5066 pass, 0 fail)
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅
- `cargo +nightly-2026-04-14 fmt --check --all` ✅
- `bun run typecheck` ✅
- `bun test` ✅ (165 pass, up from 152)