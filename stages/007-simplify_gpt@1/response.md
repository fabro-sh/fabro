Completed the review/cleanup pass. The subagent tool failed twice with an infrastructure “item not found” error before returning findings, so I proceeded with the same reuse/quality/efficiency review locally.

Fixes applied:

- Reused existing billing aggregation helper:
  - `get_run_billing` now uses `BilledTokenCounts::from_billed_usage(...)` for per-stage billing instead of duplicating token-copy logic.
- Tightened frontend typing/quality:
  - Removed unnecessary `StageState` cast/import in `run-billing.tsx`.
  - Updated billing empty-state copy from “No completed stages yet” to “No stages yet” so it matches the new in-flight behavior.
  - Ensured touched TS files end with newlines.
- Improved live ticking correctness:
  - `useTickingNow` now refreshes `Date.now()` immediately when ticking becomes active, avoiding a stale first rendered runtime if data arrives after mount.
- Strengthened reducer test coverage:
  - `stage_completed_records_duration_usage_and_terminal_state` now asserts `StageCompleted` stores billing usage, not just duration/state.

Validation run:

- `cargo +nightly-2026-04-14 fmt --all`
- `cargo nextest run -p fabro-store stage_completed_records_duration_usage_and_terminal_state stage_started_after_retrying_returns_to_running_and_resets_attempt_data`
- `cargo nextest run -p fabro-server list_run_stages_projects_retrying_until_completion run_billing_retried_node_then_succeeded_emits_one_row_with_final_attempt_duration run_billing_revisited_node_collapses_to_two_rows_with_latest_visit_data`
- `cargo nextest run -p fabro-api stage_projection_round_trips_representative_json run_billing_stage_round_trips_terminal_row_with_started_at_and_state run_billing_stage_round_trips_in_flight_row`
- `cargo check -q -p fabro-server -p fabro-store -p fabro-api`
- `cargo +nightly-2026-04-14 clippy -q -p fabro-server -p fabro-store --all-targets -- -D warnings`
- `cd apps/fabro-web && bun run typecheck`
- `cd apps/fabro-web && bun test app/routes/run-billing.test.tsx app/lib/run-events.test.tsx`
- `git diff --check`

All passed.