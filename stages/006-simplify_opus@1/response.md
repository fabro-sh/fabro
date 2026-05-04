## Summary of fixes

I aggregated findings from three review agents (reuse, quality, efficiency) and applied the highest-impact ones:

**Code reuse / quality:**
1. **Replaced `reset_for_new_attempt` with `begin_attempt`** (`run_projection.rs`): collapsed a 22-line field-by-field reset into `*self = Self::new(...)` reusing the existing constructor, then sets `started_at` and `state` in one call. Eliminates the drift risk where new fields had to be added in two places.
2. **Extracted `useTickingNow` hook** (`apps/fabro-web/app/lib/time.ts`) and used it in 3 sites: `run-billing.tsx`, `run-stages.tsx`, `stage-sidebar.tsx`. Removed three near-duplicate `useState(tick) + setInterval(1000)` patterns.
3. **Reused `IN_FLIGHT_STAGE_STATES` constant** in `stage-sidebar.ts` and used the generated `StageState` enum on the frontend instead of three raw string literals (typed against the API contract).
4. **Switched `ACTIVE_STAGE_STATES`/`SUCCEEDED_STAGE_STATES` to use `StageState.*` constants** instead of bare strings.
5. **Dropped dead fields** `MappedStageRow.inFlight`, `MappedStageRow.startedAt`, and `MappedBilling.hasInFlight` from `run-billing.tsx`.

**Efficiency:**
6. **Eliminated the `billed_usages: Vec<BilledModelUsage>` allocation and second pass** in `get_run_billing`. Grand totals are now derived in-place by summing the per-model totals already accumulated, removing N clones of `BilledModelUsage` and an N-walk of `from_billed_usage`.
7. **Reduced string allocations** in the billing handler: changed `dedupe_by_node_id` HashMap key from `String` to `&'a str` (one alloc per *unique* node, not per visit); used get-then-insert for `by_model_totals` (no key clone on hash hit); and used destructuring + move for `RunStage` fields (3 → 2 clones per row).
8. **Memoized completed rows and `modelBreakdown`** in `run-billing.tsx` so they don't re-allocate on every 1Hz tick — only the in-flight rows are re-derived against `now`.

**Test fix tagged onto behavioral change:**
9. Updated two integration tests in `tests/it/scenario/usage.rs` to expect the new `exit` row that the projection-based handler correctly surfaces (consistent with the PR plan's goal of showing all stages from the projection, not just `checkpoint.completed_nodes`).

**Skipped (non-issues or out of scope):**
- `effective_state()` `completion`-fallback: not dead code — checkpoint-derived `Skipped` stages set `completion` without `state`, the fallback handles them.
- `dedupe_by_node_id` ↔ `finalize.rs` shared helper: cross-crate refactor flagged in the original PR as out of scope.
- Trailing newlines, fixture-builder consolidation, retry-event graph invalidation: minor / debatable, not worth code churn.

**Verification:** `cargo nextest run --workspace` (5072 passed), `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` (clean), `cargo fmt --check` (clean), `bun run typecheck` (clean), `bun test` (177 passed).