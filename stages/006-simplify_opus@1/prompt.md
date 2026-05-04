Goal: # Billing & Stages: Read From Projection

## Context

The Billing tab on a running run omits the in-flight stage entirely, and the footer total runtime is frozen at the last server response.

Root cause: `GET /runs/{id}/billing` and `GET /runs/{id}/stages` (both in `lib/crates/fabro-server/src/server/handler/billing.rs`) bypass `RunProjection` and read `checkpoint.completed_nodes` + `checkpoint.node_outcomes` directly. The checkpoint only knows about *finished* nodes, so in-flight stages are invisible. `list_run_stages` had to grow a `next_node_id` workaround at `:113`; billing has no equivalent.

`RunProjection` is the canonical event-sourced read model. `StageStarted` already creates a `StageProjection` entry the moment a stage begins (`run_state.rs:289`). The projection just doesn't yet store `started_at`, completion duration, billing usage, or `state` (Retrying vs Running).

Goal: extend `StageProjection` with the missing event-derived fields, then collapse both handlers to thin views over `RunProjection.iter_stages()`. In-flight rows fall out for free. The frontend ticks runtime client-side using a server-supplied `started_at`.

Audit confirmed these are the only two read endpoints with the bypass pattern.

## Plan

### 1. Extend `StageProjection`

File: `lib/crates/fabro-types/src/run_projection.rs`

Add four fields to `StageProjection`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub started_at:  Option<DateTime<Utc>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub duration_ms: Option<u64>,
#[serde(skip)]                                  // server-internal; not on the wire
pub usage:       Option<BilledModelUsage>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub state:       Option<StageState>,
```

Why store `state` instead of deriving: the reducer needs to track `Retrying` (from `StageRetrying` events), which is not derivable from `completion` alone. Storing the field keeps the projection correct and removes the need for the existing `active_stage_state_from_events` event-replay (`billing.rs:19`). Use `Option<_>` so old serialized projections deserialize as `None` and can fall through a derivation helper.

Why `usage` is `#[serde(skip)]`: `BilledModelUsage` has no OpenAPI schema today (only `BilledTokenCounts` does, at `fabro-api.yaml:5756`). Modeling the full nested usage shape is out of scope for this PR, and `/runs/{id}/state` consumers can hit `/billing` if they need per-stage tokens. The billing handler reads `stage.usage` in-process to build `RunBillingStage.billing`. The field still survives in-process projection rebuild because `apply_event` reapplies it from `StageCompletedProps.billing` on every load.

Helper methods:

```rust
pub fn effective_state(&self) -> StageState {
    self.state.unwrap_or_else(|| match &self.completion {
        Some(c) => StageState::from(c.outcome),
        None => StageState::Running,
    })
}

pub fn runtime_secs(&self, now: DateTime<Utc>) -> Option<f64> {
    // Live state ticks; only use stored duration_ms once terminal.
    // This handles retries safely: even if a previous failed attempt left
    // `duration_ms` set, the new `state = Running` makes us recompute live.
    let state = self.effective_state();
    if matches!(state, StageState::Running | StageState::Retrying | StageState::Pending) {
        return self.started_at.map(|started| {
            now.signed_duration_since(started)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0
        });
    }
    self.duration_ms.map(|ms| ms as f64 / 1000.0)
}
```

`effective_state` keeps old serialized projections working without a backfill.

Update `StageProjection::new` to default the four new fields to `None`.

### 2. Capture the new fields in the reducer

File: `lib/crates/fabro-store/src/run_state.rs`. The reducer already has `let ts = stored.ts` in scope at `:46`.

- `StageStarted` arm (`:289`): add a `StageProjection::reset_for_new_attempt(&mut self)` helper and call it after `stage_entry(...)`, then set `stage.started_at = Some(ts)` and `stage.state = Some(StageState::Running)`.

  `reset_for_new_attempt` clears **every attempt-result field**, because all of them are repopulated by per-attempt lifecycle events (`run_state.rs:299, 306, 312, 324, 338, 344, 350, 359, 375`) and would otherwise leak prior-attempt data on retry:

  - `completion`, `duration_ms`, `usage`, `state` (terminal data)
  - `response`, `prompt`, `provider_used`, `diff` (LLM/agent attempt data)
  - `script_invocation`, `script_timing`, `parallel_results` (handler attempt data)
  - `stdout`, `stderr`, `stdout_bytes`, `stderr_bytes`, `streams_separated`, `live_streaming`, `termination` (command-output attempt data)

  The only fields preserved are `first_event_seq` (identity / sort key, set on first creation) and `started_at` / `state` which are written immediately after the reset. Without this reset, a retry with reused visit would leave `state = Running` alongside `completion.outcome = Failed` and prior `stdout`/`stderr` content — inconsistent projection state visible via `/runs/{id}/state`.
- `StageCompleted` arm (`:312`): set `stage.duration_ms = Some(props.duration_ms)`, `stage.usage = props.billing.clone()`, `stage.state = Some(StageState::from(stage_outcome_from_props(props).status))`.
- `StageFailed` arm (`:324`): set `stage.duration_ms = Some(props.duration_ms)` and `stage.state = Some(StageState::Failed)`.
- `StageRetrying` arm: new — locate stage at current visit, set `stage.state = Some(StageState::Retrying)`. (No corresponding handler exists today.)

Add unit tests in the existing `#[cfg(test)] mod tests` block for each arm and one transition test (`StageStarted → StageFailed → StageRetrying → StageStarted` returns to `Running`).

### 3. Rewrite `get_run_billing`

File: `lib/crates/fabro-server/src/server/handler/billing.rs:128`

Replace the `checkpoint.completed_nodes` loop (`:179`) with:

1. Load `RunProjection` once (already done at `:140`).
2. Capture `now: DateTime<Utc>` once.
3. Collect `(StageId, &StageProjection)` from `projection.iter_stages()` into a `Vec`.
4. Aggregate by `node_id` to align with finalized output (`fabro-workflow/src/pipeline/finalize.rs:113`):
   - **Order**: first occurrence wins. For each `node_id`, the sort key is the **minimum** `first_event_seq` across all of that node's visits (i.e. when the node first appeared in the event log).
   - **Data**: latest visit wins. The displayed row uses fields from the entry with the largest `visit` for that node_id.
   - This produces the same A, B order for an A→B→A loop that finalize produces. The current live handler iterates `checkpoint.completed_nodes: Vec<String>` directly and could emit duplicate rows for revisits; the new behavior collapses them, intentionally matching finalize.
5. Sort the deduped rows by the per-node_id minimum `first_event_seq` from step 4.
6. For each stage, build a `RunBillingStage`:
   - `stage`: `BillingStageRef { id, name = node_id }`.
   - `model`: from `stage.usage.as_ref().map(|u| ModelReference { id: u.model_id().to_string() })`.
   - `billing`: from `stage.usage` via the existing `BilledTokenCounts` shape; default if `None`.
   - `runtime_secs`: `stage.runtime_secs(now).unwrap_or(0.0)`.
   - `started_at`: `stage.started_at` (new field — see §5).
   - `state`: `stage.effective_state()` (new field — see §5).
7. Totals: server-side total `runtime_secs` sums all rendered row runtimes (now includes the in-flight row's elapsed time). Tokens & cost via `BilledTokenCounts::from_billed_usage` over completed-stage usage — same as today.
8. By-model breakdown: same as today, built from projection-derived usage list.

Drop the dependency on `fabro_workflow::extract_stage_durations_from_events` from this handler.

### 4. Rewrite `list_run_stages`

Same handler, `:38`.

Same shape as §3 for `RunStage`:

- Iterate `projection.iter_stages()`, dedupe by node_id with the same rule as §3 step 4: latest-visit data, sort by per-node_id minimum `first_event_seq`.
- `RunStage { id, name, status: stage.effective_state(), duration_secs: stage.runtime_secs(now), dot_id: Some(node_id), started_at: stage.started_at }`.
- Drop the `next_node_id` synthesis at `:113`.
- Drop the live-vs-store fork at `:50–78`; the projection is updated as events are written, so a single `state.store.open_run_reader(...).state()` read suffices.
- Delete `active_stage_state_from_events` at `:19` — no longer needed; `state` is on the projection.

### 5. OpenAPI: extend three schemas

File: `docs/public/api-reference/fabro-api.yaml`

- **`RunBillingStage`** (`:6610`): add optional `started_at: string (date-time)` and `state: $ref StageState`. Frontend uses `state` to detect in-flight rows.
- **`RunStage`** (`:6316`): add optional `started_at: string (date-time)`. `status: StageState` already exists.
- **`StageProjection`** (`:5279`): add optional `started_at`, `duration_ms`, and `state: StageState`. **Do not** add `usage` here — the field is `#[serde(skip)]` server-internal (see §1). `BilledModelUsage` is not currently an OpenAPI schema and modeling it would balloon this PR's surface; `/runs/{id}/state` consumers needing per-stage tokens hit `/billing` instead.

After editing: `cargo build -p fabro-api` regenerates Rust types; `cd lib/packages/fabro-api-client && bun run generate` regenerates the TS client.

### 6. Update demo fixtures

File: `lib/crates/fabro-server/src/demo/mod.rs`

- `RunStage` literals at `:1184, 1191, 1198, 1205` — add `started_at: None`.
- `RunBillingStage` literals at `:1233, 1252, 1271, 1290` — add `started_at: None` and `state: StageState::Succeeded` (or appropriate per fixture).
- Any `StageProjection` literals in tests/fixtures — search `rg "StageProjection \{"` and add the new optional fields (typically `..Default::default()` shape if used).

### 7. Frontend: invalidate on stage events + live tick

Files: `apps/fabro-web/app/lib/run-events.ts`, `apps/fabro-web/app/routes/run-billing.tsx`.

`run-events.ts`:
- Add `"stage.retrying"` to the `STAGE_EVENTS` set at `:35`. The projection now stores Retrying state, so the UI must refetch when this event arrives.
- Add `queryKeys.runs.billing(runId)` to the `STAGE_EVENTS` invalidation list at `:75`.
- Update the `queryKeysForRunEvent` test in `run-events.test.tsx` to verify `stage.retrying` invalidates stages, billing, events, and (when stage_id present) stage turns.

`run-billing.tsx`:
- Detect in-flight via the new `state` field: `state === "running" || state === "retrying"`.
- If any row is in-flight, run a `useEffect` `setInterval(..., 1000)` that bumps a `now` state. Render the in-flight row's runtime as `(now − new Date(started_at)) / 1000`.
- **Footer total**: while ticking, derive total from the rendered row runtimes — sum up the displayed seconds (which now include the live elapsed for the in-flight row). Otherwise (terminal run) use `billing.totals.runtime_secs` from the server.
- Drop the empty-state at `:83` when any in-flight row exists; the table appears as soon as the first stage starts.

Update `apps/fabro-web/app/routes/run-billing.test.tsx`:
- Extend fixtures with `started_at` and `state`.
- Add a test for an in-flight row (state = `running`) that asserts (a) the row renders, (b) the footer total includes the elapsed time, (c) the table is shown even when no stage has completed.

### 8. What stays out of scope

- **Live tokens during a stage.** Requires a new `agent.turn.completed { usage }` event from `fabro-agent`/`fabro-llm` plus a reducer arm to accumulate onto `StageProjection.usage`. The schema in §1 is ready; instrumenting it is a separate change.
- **Per-visit billing rows.** Today's behavior aggregates by node_id (latest visit). One row per retry/revisit is a UX decision separate from this fix.
- **Removing `checkpoint.node_outcomes`.** Still used by workflow execution: `artifact.rs:92,134`, `finalize.rs:119,394`, retro/conditionals. Leave it.
- **Mixed in-memory/projection reads on `/checkpoint` and `/graph`.** Different shape of issue; not this PR.

### 9. API round-trip tests

Files: `lib/crates/fabro-api/tests/stage_projection_round_trip.rs`, `lib/crates/fabro-api/tests/run_billing_stage_round_trip.rs`.

Extend the representative-JSON cases:

- `stage_projection_round_trip.rs`: add `started_at`, `duration_ms`, `state` to the JSON fixture and assert they round-trip. Confirms the OpenAPI schema and Rust type stay in lock-step for the new fields.
- `run_billing_stage_round_trip.rs`: add `started_at` and `state` to the JSON fixture and assert they round-trip. Add a second case for an in-flight row (`state = "running"`, no `model`, zero `billing`).

These prevent silent drift if the OpenAPI schema and Rust type ever diverge on the new fields.

## Files to modify

- `lib/crates/fabro-types/src/run_projection.rs` — fields + helpers
- `lib/crates/fabro-store/src/run_state.rs` — reducer arms (incl. new `StageRetrying`) + tests
- `lib/crates/fabro-server/src/server/handler/billing.rs` — both handlers rewritten; delete `active_stage_state_from_events`
- `lib/crates/fabro-server/src/server/tests.rs` — keep `list_run_stages_projects_retrying_until_completion`; verify it still passes via the new projection-based path
- `lib/crates/fabro-server/src/demo/mod.rs` — fixture updates
- `docs/public/api-reference/fabro-api.yaml` — `RunBillingStage`, `RunStage`, `StageProjection`
- `lib/packages/fabro-api-client` — regenerated
- `apps/fabro-web/app/lib/run-events.ts` — billing invalidation on stage events
- `apps/fabro-web/app/routes/run-billing.tsx` — in-flight detection + tick + derived footer total
- `apps/fabro-web/app/routes/run-billing.test.tsx` — new fixtures + in-flight + footer-tick assertions
- `lib/crates/fabro-api/tests/stage_projection_round_trip.rs` — extend fixture with new fields
- `lib/crates/fabro-api/tests/run_billing_stage_round_trip.rs` — extend fixture with new fields, add in-flight case
- `apps/fabro-web/app/lib/run-events.test.tsx` — assert `stage.retrying` invalidates billing/stages/events

## Existing utilities to reuse

- `RunProjection::iter_stages()` — `lib/crates/fabro-types/src/run_projection.rs:102`
- `StageProjection::first_event_seq` — already a `NonZeroU32`, ready as sort key
- `StageState` — `lib/crates/fabro-types/src/outcome.rs:111` with `From<StageOutcome>` already wired
- `BilledTokenCounts::from_billed_usage` — used by current totals path
- `accumulate_model_billing` — `lib/crates/fabro-server/src/server.rs:539`, used for by-model breakdown
- chrono pattern: `now.signed_duration_since(...).num_milliseconds().max(0) as f64 / 1000.0` (e.g. `lib/crates/fabro-cli/src/commands/runs/list.rs:99`)

## Verification

1. **Reducer unit tests** in `run_state.rs`:
   - `stage_started_records_started_at_and_running_state`
   - `stage_completed_records_duration_usage_and_terminal_state`
   - `stage_failed_records_duration_and_failed_state`
   - `stage_retrying_sets_retrying_state`
   - `stage_started_after_retrying_returns_to_running` (transition)
2. **Existing test must still pass**: `list_run_stages_projects_retrying_until_completion` (`server/tests.rs:2126`) — covers Retrying via the new projection path.
3. **New handler integration tests** in `lib/crates/fabro-server/tests/it/scenario/usage.rs`:
   - **Mid-run snapshot**: pause workflow with one completed and one in-flight stage; assert `/billing` returns two rows; in-flight row has `state = "running"`, `model = null`, zero `billing` tokens, non-zero `runtime_secs`; totals include the in-flight runtime.
   - **Retried node, mid-retry**: StageStarted → StageFailed (duration_ms = 10) → StageRetrying → StageStarted (no completion yet); assert the row's `state = "running"` and `runtime_secs` reflects elapsed since the **second** StageStarted, not the failed attempt's 10ms. Pin the regression risk that motivated the `runtime_secs()` priority inversion.
   - **Retried node, succeeded**: same prefix → StageCompleted; assert one row per node_id (latest visit), state `Succeeded`, duration = final attempt's `duration_ms`.
   - **Revisited node (loop, multi-node)**: emit A completed → B completed → A revisited+completed (visit=2). Assert (a) two rows total, (b) order is A, B (matches `finalize.rs:113`), (c) A's row carries the latest visit's data (visit=2 duration/usage), not the first visit's. Pins both the dedupe rule and the ordering rule against future drift.
4. **Frontend tests** — `run-billing.test.tsx`:
   - In-flight row renders with runtime > 0.
   - Footer total ticks while the in-flight row ticks.
   - Empty-state hidden when an in-flight row exists.
5. **End-to-end smoke** — `fabro run repl`, open `/runs/<id>/billing` in dev:
   - In-flight stage row appears immediately on `stage.started`.
   - Runtime ticks once per second.
   - On `stage.completed`, row gets `duration_ms` + tokens; next stage's row appears.
   - Footer reflects live in-flight runtime.
6. **Conformance** — `cargo nextest run -p fabro-server`, `cd apps/fabro-web && bun run typecheck && bun test`, `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`. Run `cargo insta pending-snapshots` afterwards in case any snapshot tests pick up the new optional fields.

## Unresolved questions

- For runs with retried/revisited nodes, is "latest visit per node_id" the right billing display, or should we eventually expose all visits as separate rows? Plan matches current behavior; flagging for future.
- `StageProjection.usage` is server-internal (`#[serde(skip)]`) for this PR. If a future consumer of `/runs/{id}/state` needs per-stage tokens, we'd model `BilledModelUsage` as an OpenAPI schema and unskip it — separate change.


## Completed stages
- **toolchain**: succeeded
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.95.0 (f2d3ce0bd 2026-03-21)
    ```
  - Stderr: (empty)
- **preflight_compile**: succeeded
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: succeeded
  - Script: `cargo +nightly-2026-04-14 clippy -q --workspace --all-targets -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: succeeded
  - Model: claude-opus-4-7, 212.1k tokens in / 72.4k out
  - Files: /home/daytona/workspace/apps/fabro-web/app/lib/query-keys.test.ts, /home/daytona/workspace/apps/fabro-web/app/lib/run-events.test.tsx, /home/daytona/workspace/apps/fabro-web/app/lib/run-events.ts, /home/daytona/workspace/apps/fabro-web/app/routes/run-billing.test.tsx, /home/daytona/workspace/apps/fabro-web/app/routes/run-billing.tsx, /home/daytona/workspace/docs/public/api-reference/fabro-api.yaml, /home/daytona/workspace/lib/crates/fabro-api/tests/run_billing_stage_round_trip.rs, /home/daytona/workspace/lib/crates/fabro-api/tests/stage_projection_round_trip.rs, /home/daytona/workspace/lib/crates/fabro-server/src/demo/mod.rs, /home/daytona/workspace/lib/crates/fabro-server/src/server/handler/billing.rs, /home/daytona/workspace/lib/crates/fabro-server/src/server/tests.rs, /home/daytona/workspace/lib/crates/fabro-server/tests/it/scenario/usage.rs, /home/daytona/workspace/lib/crates/fabro-store/src/run_state.rs, /home/daytona/workspace/lib/crates/fabro-types/src/run_projection.rs, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/run-billing-stage.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/run-stage.ts, /home/daytona/workspace/lib/packages/fabro-api-client/src/models/stage-projection.ts


# Simplify: Code Review and Cleanup

Review changes vs. origin for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run git diff (or git diff HEAD if there are staged changes) to see what changed. If there are no git changes, review the most recently modified files that the user mentioned or that you edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message. Pass each agent the full diff so it has the complete context.

### Agent 1: Code Reuse Review

For each change:

1. Search for existing utilities and helpers that could replace newly written code. Use Grep to find similar patterns elsewhere in the codebase — common locations are utility directories, shared modules, and files adjacent to the changed ones.
2. Flag any new function that duplicates existing functionality. Suggest the existing function to use instead.
3. Flag any inline logic that could use an existing utility — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards, and similar patterns are common candidates.

Note: This is a greenfield app, so focus on maximizing simplicity and don't worry about changing things to achieve it.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. Redundant state: state that duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. Parameter sprawl: adding new parameters to a function instead of generalizing or restructuring existing ones
3. Copy-paste with slight variation: near-duplicate code blocks that should be unified with a shared abstraction
4. Leaky abstractions: exposing internal details that should be encapsulated, or breaking existing abstraction boundaries
5. Stringly-typed code: using raw strings where constants, enums (string unions), or branded types already exist in the codebase

Note: This is a greenfield app, so be aggressive in optimizing quality.

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. Unnecessary work: redundant computations, repeated file reads, duplicate network/API calls, N+1 patterns
2. Missed concurrency: independent operations run sequentially when they could run in parallel
3. Hot-path bloat: new blocking work added to startup or per-request/per-render hot paths
4. Unnecessary existence checks: pre-checking file/resource existence before operating (TOCTOU anti-pattern) — operate directly and handle the error
5. Memory: unbounded data structures, missing cleanup, event listener leaks
6. Overly broad operations: reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate their findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue with the finding, just skip it.

When done, briefly summarize what was fixed (or confirm the code was already clean).