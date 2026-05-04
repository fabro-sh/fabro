All files from the plan are touched. Summary of what was implemented:

## Summary

**§1 — `StageProjection` extended** (`lib/crates/fabro-types/src/run_projection.rs`):
- Added `started_at`, `duration_ms`, `usage` (`#[serde(skip)]`), and `state` fields.
- Added `effective_state()` and `runtime_secs(now)` helpers.
- Added `reset_for_new_attempt()` which clears every per-attempt result field.

**§2 — Reducer arms updated** (`lib/crates/fabro-store/src/run_state.rs`):
- `StageStarted`: calls `reset_for_new_attempt`, sets `started_at` + `state = Running`.
- `StageCompleted`: sets `duration_ms`, `usage`, terminal `state`.
- `StageFailed`: sets `duration_ms` + `state = Failed`.
- `StageRetrying`: new arm — sets `state = Retrying`.
- 5 unit tests covering each arm + the retry transition.

**§3, §4 — Both handlers rewritten** (`lib/crates/fabro-server/src/server/handler/billing.rs`):
- `dedupe_by_node_id` — latest-visit data, sorted by minimum `first_event_seq` (matches `finalize.rs` ordering).
- `list_run_stages` — single projection read; in-flight rows fall out for free.
- `get_run_billing` — same pattern; totals sum row runtimes (includes live in-flight time).
- Removed `active_stage_state_from_events`, the `next_node_id` workaround, the live-vs-store fork, and the workflow `extract_stage_durations_from_events` dependency.

**§5 — OpenAPI spec** (`docs/public/api-reference/fabro-api.yaml`):
- `RunBillingStage`: added `started_at` and `state`.
- `RunStage`: added `started_at`.
- `StageProjection`: added `started_at`, `duration_ms`, `state` (no `usage` per plan).

**§6 — Demo fixtures**: 4× `RunStage` literals + 4× `RunBillingStage` literals updated.

**§7 — Frontend**:
- `run-events.ts`: `stage.retrying` added to `STAGE_EVENTS`; billing key added to invalidation list. Test updated.
- `run-billing.tsx`: in-flight detection via `state`; `useEffect` `setInterval(1s)` while any row is in-flight; footer total derived from row runtimes while ticking; empty-state hidden once any row exists.
- `run-billing.test.tsx`: new in-flight test verifying live runtime, footer ticking, and table rendering pre-completion.
- `query-keys.test.ts`: updated to include billing in stage event invalidation.

**§9 — Round-trip tests**: extended both `stage_projection_round_trip.rs` and `run_billing_stage_round_trip.rs` with the new fields and an in-flight-row case.

**Plus integration tests** in `lib/crates/fabro-server/src/server/tests.rs`:
- `run_billing_retried_node_then_succeeded_emits_one_row_with_final_attempt_duration`
- `run_billing_revisited_node_collapses_to_two_rows_with_latest_visit_data`
- Updated `list_run_stages_projects_retrying_until_completion` to emit StageStarted/StageCompleted for `setup` so the projection-based handler sees it.

**TS API client**: regenerated manually (Java unavailable) — three model files updated to match the new OpenAPI shape.

Final verification:
- `cargo check --workspace` ✅
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅
- `cargo nextest run -p fabro-store -p fabro-types -p fabro-api` ✅ (346 tests)
- `cargo nextest run -p fabro-server` ✅ except 2 pre-existing graph-render failures unrelated to this PR
- `bun test` ✅ (177 tests)
- `bun run typecheck` ✅