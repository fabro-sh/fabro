## Summary of cleanup fixes

I aggregated findings from three parallel review agents (reuse, quality, efficiency) and applied 6 fixes. Skipped the test-builder duplication, the `Stage.id` vs `nodeId+visit` redundancy, and the trailing-newline notes — those are larger refactors out of scope for this pass.

### Fixed

1. **Eliminated JSON round-trip in duration extraction** (`lib/crates/fabro-workflow/src/lib.rs`)
   - Both `extract_stage_durations_from_events` and `extract_stage_durations_by_stage_id` were serializing each event to `serde_json::Value` and looking up `"duration_ms"` as `u64`. Replaced with a typed `EventBody::StageCompleted(props) | StageFailed(props) => Some(props.duration_ms)` match in a shared helper. Removes per-event JSON allocation on a per-request hot path and removes the stringly-typed `event_name() != "stage.completed"` compares.

2. **Fixed O(stages × events) in `list_run_stages`** (`lib/crates/fabro-server/src/server/handler/billing.rs`)
   - Replaced `stage_status_from_events(events, stage_id, projection)` (called per stage, each scanning the full event log) with a single forward pass `latest_stage_states(events) -> HashMap<StageId, StageState>` that overwrites earlier lifecycle entries. Per-stage lookup is now O(1).
   - Dropped the dead `_ => StageState::Pending` arm in the lifecycle match (unreachable after the prefilter) by inverting the helper to return `Option<StageState>`.
   - Deduplicated the double `stage_id.node_id().to_string()` allocation per stage.

3. **Extracted `formatStageLabel` helper** (`apps/fabro-web/app/lib/stage-sidebar.ts`)
   - The `stage.visit > 1 ? \`${name} (${visit})\` : name` formula was duplicated in `stage-sidebar.tsx` and `run-stages.tsx`. Now lives in `stage-sidebar.ts` and is used in both places.

4. **Single-pass graph aggregation** (`apps/fabro-web/app/lib/stage-sidebar.ts`)
   - `aggregateGraphNodeStatus` was building a `Map<nodeId, Stage[]>`, sorting each list, then doing `[...list].reverse().find(...)` to find the latest active visit. Replaced with a single pass tracking `latest` and `latestActive` per nodeId — no sort, no array copy, no spread.

5. **`stageIdFromPayload` correctness fix** (`apps/fabro-web/app/lib/run-events.ts`)
   - The fallback returned a bare `node_id` (e.g. `"verify"`) when `stage_id` was absent. Under the new scheme, cache keys are suffixed (`stageTurns(runId, "verify@1")`), so a bare `node_id` never matches and invalidations would silently no-op. Now returns `undefined` if `stage_id` isn't present, which matches the existing test assertion that bare `verify` should NOT appear in invalidated keys.

### Verification

- `cargo nextest run -p fabro-server -p fabro-workflow` → 1498 passed
- `cargo clippy --all-targets -D warnings` (changed crates) → clean
- `cd apps/fabro-web && bun run typecheck` → clean
- `cd apps/fabro-web && bun test` → 165 passed