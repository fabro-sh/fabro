Goal: # Stage URLs encode visit (`node@visit`)

## Context

Today, in the Fabro web UI, stages that re-run (e.g. `verify` in a loop) all
collapse to the same URL: `/runs/{id}/stages/verify`. The sidebar lists them
multiple times but every link/selection points at the first visit.

A **Stage** is a Node + a Visit (1-indexed; bumped each time the workflow
re-enters that node). The data model already knows this:
`StageId(node_id, visit)` exists in `lib/crates/fabro-types/src/stage_id.rs`,
and the OpenAPI `StageId` path parameter
(`docs/public/api-reference/fabro-api.yaml:2925`) already documents the
`node_id@visit` form. The bug is that `GET /api/v1/runs/{id}/stages` returns
`RunStage.id = node_id` (no visit suffix), and the events fallback in the UI
filters by `node_id` instead of the full `stage_id`.

Note: "visit" is deliberate. There is a separate retry-attempt counter
inside a single visit (`StageStartedProps.attempt` in
`lib/crates/fabro-types/src/run_event/stage.rs:13`) — that's not what we're
modeling here. URLs and the new field both refer to **visits**.

Outcome: each stage gets a distinct URL (e.g. `verify@1`, `verify@2`) that
loads only that visit's turns/logs, with a `(N)` indicator in the sidebar
when `N > 1`.

## Approach

**Server**: rebuild the stage list from `RunProjection::iter_stages()`, which
is already keyed by full `StageId` (`HashMap<StageId, StageProjection>` in
`lib/crates/fabro-types/src/run_projection.rs:32`). This deletes the
`checkpoint.completed_nodes` walk (which loses visit info — it's a
`Vec<String>` of node_ids only) and the `next_node_id` branch entirely.

**Status derivation is event-driven, not completion-driven.**
`StageProjection.completion` is set by `StageFailed` *even when* the workflow
is about to retry (`run_state.rs:329` — `StageRetrying` does not clear it),
so reading completion alone would show `failed` for a stage that's
retrying. For each stage, scan its events (filtered by exact `stage_id`)
and take the **latest** lifecycle event:
- `stage.retrying` → `StageState::Retrying`
- `stage.failed(props)` with `props.will_retry == true` → `StageState::Retrying`
- `stage.failed(props)` with `props.will_retry == false` → `StageState::Failed`
- `stage.completed` → `StageState::from(StageCompletedProps.status)`
- `stage.started` (no later completed/failed/retrying) → `StageState::Running`

Use the projection's `completion` only as a tiebreaker when no lifecycle
events for that stage_id exist (defensive case). The
`StageState::from(StageOutcome)` impl is at
`lib/crates/fabro-types/src/outcome.rs:136`.

**API contract**: on `RunStage`, add a required `visit: integer` field, and
**rename `dot_id` → `node_id`** (required) for consistency with
`StageId::node_id()`, `EventEnvelope.node_id`, and the rest of the type
vocabulary. Tighten the `id` description to call out the `node_id@visit`
form. This is a breaking field rename; per project policy
("simplest change possible, we don't care about migration"), we do it now
rather than carrying both names.

**Frontend**: links and selection already use `stage.id`, so they propagate
naturally once the API returns `verify@1`/`verify@2`. The events-fallback
filter switches from `e.node_id === stageId` to `e.stage_id === stageId`.
Sidebar/header append `(N)` only when `visit > 1`. The
`isVisibleStage(stage.id)` filter switches to `isVisibleStage(stage.node_id)`
so that `start@1`/`exit@1` are still hidden.

One fixture run with two visits of the same node is added to demo data so
this code path stays under test.

## Files to change (in order)

### 1. OpenAPI — `docs/public/api-reference/fabro-api.yaml`

`RunStage` schema (line 6315):
- `id`: clarify description: `StageId in "node_id@visit" form, e.g. verify@2`. Update example to `verify@2`.
- Add `visit: { type: integer, format: uint32, minimum: 1, description: "1-based visit count; bumped each time the workflow re-enters this node" }`. Mark required. (`format: uint32` + `minimum: 1` codegens to `NonZeroU32`, matching `StageProjection.first_event_seq` at line 5286–5288 of the spec.)
- **Rename `dot_id` → `node_id`** and mark required. Description: "Node id in the workflow graph; multiple stages with different visits share the same node_id." Example: `verify`.

### 2. Generated code

- `cargo build -p fabro-api` — regenerates Rust types via `progenitor`.
- `cd lib/packages/fabro-api-client && bun run generate` — regenerates TS client.

### 3. Workflow lib — `lib/crates/fabro-workflow/src/lib.rs`

Add a sibling to `extract_stage_durations_from_events` (line 89). Leave the
existing function alone — `finalize.rs:81,506` and `retro.rs:56` operate on a
single visit per node and shouldn't change. New function:

```rust
pub fn extract_stage_durations_by_stage_id(
    events: &[EventEnvelope],
) -> HashMap<StageId, u64>
```

Filters `stage.completed`/`stage.failed`, keys by `envelope.stage_id`.

### 4. Server handler — `lib/crates/fabro-server/src/server/handler/billing.rs`

Rewrite `list_run_stages` (lines 38–126):

- Replace `checkpoint.completed_nodes` iteration with
  `projection.iter_stages()`, collected and sorted by `first_event_seq`.
- Per stage, build `RunStage`:
  - `id = stage_id.to_string()`
  - `node_id = stage_id.node_id().to_string()`
  - `name = stage_id.node_id().to_string()` (UI adds the suffix)
  - `visit = NonZeroU32::new(stage_id.visit()).expect("StageId.visit is 1-based")`
    (generated type is `NonZeroU32` because of `format: uint32` + `minimum: 1`)
  - `status = stage_status_from_events(events, &stage_id, &projection)`
    (see Status derivation below)
  - `duration_secs`: from the new `extract_stage_durations_by_stage_id`.
- **Status derivation** — replace `active_stage_state_from_events` (line 19)
  with `stage_status_from_events(events: &[EventEnvelope], stage_id: &StageId,
  projection: &RunProjection) -> StageState`. Implementation:
  1. Filter events to those with `envelope.event.stage_id == Some(stage_id)`.
  2. Find the **latest** lifecycle event among `stage.started`,
     `stage.retrying`, `stage.completed`, `stage.failed` for that stage_id.
  3. Map:
     - `stage.started` → `Running`
     - `stage.retrying` → `Retrying`
     - `stage.failed(props)` with `props.will_retry == true` → `Retrying`
       (a will-retry failure is conceptually mid-retry, even before the
       `stage.retrying` envelope lands; field defined at
       `lib/crates/fabro-types/src/run_event/stage.rs:57`)
     - `stage.failed(props)` with `props.will_retry == false` → `Failed`
     - `stage.completed` → `StageState::from(StageCompletedProps.status)`
       (using the existing `From<StageOutcome> for StageState` impl)
  4. Fallback: if no lifecycle events, use
     `StageState::from(completion.outcome)` from the projection if present,
     else `Pending`.
- Drop the `next_node_id` branch (lines 113–123) entirely — the projection
  now carries the in-flight stage.

### 5. Demo fixtures — `lib/crates/fabro-server/src/demo/mod.rs:1156`

Suffix existing four IDs with `@1` and set `visit: 1`. Add a 5th entry to
model a re-run:

```rust
fn visit(n: u32) -> NonZeroU32 { NonZeroU32::new(n).expect("visit is 1-based") }

RunStage { id: "apply-changes@1".into(), name: "apply-changes".into(),
           status: Succeeded, duration_secs: Some(118.0),
           node_id: "apply".into(), visit: visit(1) },
RunStage { id: "apply-changes@2".into(), name: "apply-changes".into(),
           status: Running, duration_secs: None,
           node_id: "apply".into(), visit: visit(2) },
```

`visit` codegens to `NonZeroU32` (see Section 1) — direct `1`/`2` literals
won't compile. Use the helper above (or inline
`NonZeroU32::new(n).unwrap()`).

Both share `node_id: "apply"` so the graph node lights up regardless of
selection.

### 6. Frontend mapping — `apps/fabro-web/app/lib/stage-sidebar.ts:13`

- Pass through `visit` and `node_id` (renamed from `dot_id`) to the sidebar
  `Stage` shape.
- Change filter to `isVisibleStage(stage.node_id)` (line 17) so suffixed IDs
  still hide `start`/`exit`.
- Drop the `?? stage.id` fallback (line 21) — `node_id` is now required.

### 7. Sidebar component — `apps/fabro-web/app/components/stage-sidebar.tsx`

- Rename the `dotId` field on `Stage` to `nodeId` (line 21).
- Add `visit: number` to the `Stage` interface (line 16).
- Render display label as `${stage.name}` when `visit <= 1`, otherwise
  `${stage.name} (${visit})` in the `<span>` at line 103.
- Update any callers reading `stage.dotId` (graph highlighting) to
  `stage.nodeId`.

### 8. SSE cache invalidation — `apps/fabro-web/app/lib/run-events.ts`

Two fixes here:

1. **Suffixed StageId routing** (`:132`): `stageIdFromPayload` currently
   returns `payload.node_id`. Once
   `queryKeys.runs.stageTurns(runId, stageId)` is keyed by `verify@1` (lines
   84, 95 of the same file), invalidations passing `verify` won't match.
   - Add `stage_id?: string` to `RunEventPayload` (line 14).
   - In `stageIdFromPayload`: prefer `payload.stage_id`; fall back to
     `payload.node_id` for events that don't carry the full StageId (e.g.
     pre-stage envelopes).
2. **Add `stage.retrying` to `STAGE_EVENTS`** (line 35). Today the set is
   `["stage.started", "stage.completed", "stage.failed"]`. The new
   server-side status logic relies on `stage.retrying`, and the workflow
   already emits it (`lib/crates/fabro-workflow/src/lifecycle/event.rs:232`).
   Without this, a selected stage stays visually `failed` until another
   invalidating event arrives — defeats the P1 fix above.

Tests:
- An envelope with `stage_id: "verify@2"` and `event: "stage.retrying"`
  invalidates `stages`, `events`, `graph`, run `detail`, and
  `stageTurns(runId, "verify@2")`.

### 9. Run-stages route — `apps/fabro-web/app/routes/run-stages.tsx`

- Line 72: change `events.filter((e) => e.node_id === stageId)` to
  `events.filter((e) => e.stage_id === stageId)`. The filter narrows the
  scope so `stageId` (the function parameter) is the authoritative StageId
  inside the loop.
- Lines 117 and 126: drop the `` ?? `${stageId}@1` `` fallback. Note:
  `EventEnvelope.stage_id` is generated as `string | null | undefined`
  (see `lib/packages/fabro-api-client/src/models/run-event.ts:34`), so
  assigning `stageId: e.stage_id` directly fails typecheck. Use the
  function parameter instead — after the filter, all surviving events have
  `stage_id === stageId` by construction:
  ```ts
  pendingCommand = { stageId, script, language };
  ...
  turns.push({ kind: "command", stageId, ... });
  ```
- Header (line ~640): when `selectedStage.visit > 1`, render
  `${selectedStage.name} (${selectedStage.visit})`.

### 10. Graph aggregation policy — `apps/fabro-web/app/routes/run-overview.tsx:66-77`

Today the graph code maps `Map<dotId, stageId>`; with two visits sharing a
node_id, the second entry silently overwrites the first, and the status
sets union all visits. Make the policy explicit:

- **Click target**: open the **latest** visit for that node_id (highest
  `visit`). Build the map deterministically — `Map.set(nodeId, latestStage.id)`
  after sorting visits ascending.
- **Status policy**: *latest visit wins for terminal states; active states
  win globally.* That is — for a given node, if any visit is `running` or
  `retrying`, the node renders that active state. Otherwise the node renders
  the **latest visit's** terminal state. So:
  - `(failed, running)` → `running` (active wins)
  - `(failed, succeeded)` → `succeeded` (latest visit wins; failure-then-fix
    should look healed, not failed)
  - `(succeeded, failed)` → `failed` (latest visit wins)
  - `(running, retrying)` → `retrying` (active; pick the latest)
- The current if/else cascade in run-overview.tsx orders running before
  failed unconditionally — switch it to a two-step compute: pick the
  display status per node by the rule above, *then* render once.
- **Frontend tests**: `(failed, running)` → running color, click → `verify@2`.
  `(failed, succeeded)` → succeeded color, click → `verify@2`.
  `(succeeded, failed)` → failed color, click → `verify@2`.

### 11. Tests

- **`lib/crates/fabro-server/src/server/tests.rs`** (alongside existing
  `list_run_stages_projects_retrying_until_completion` at line 2126): add
  `list_run_stages_distinguishes_visits` — build a run with two visits of
  the same node, hit `GET /runs/{id}/stages`, assert two `RunStage`
  entries with distinct `id`/`visit` and the same `node_id`.
- **`lib/crates/fabro-server/src/server/tests.rs`**:
  `list_run_stages_shows_retrying_after_failed_event` — a stage where the
  latest event is `stage.failed` followed by `stage.retrying` renders as
  `Retrying`, not `Failed`.
- **`lib/crates/fabro-server/src/server/tests.rs`**:
  `list_run_stages_shows_retrying_when_failed_will_retry` — a stage whose
  *only* lifecycle event so far is `stage.failed { will_retry: true }`
  (no `stage.retrying` envelope yet) still renders as `Retrying`. Narrower
  guard for the will_retry branch.
- **`apps/fabro-web/app/routes/run-stages.test.ts`**: `turnsFromEvents`
  filters correctly on `stage_id` (verify@1 events vs verify@2 events do
  not cross-contaminate).
- **`apps/fabro-web/app/lib/stage-sidebar.test.ts`** (new or existing): map
  fixture with two `apply-changes` visits → two distinct sidebar entries,
  display labels `apply-changes` and `apply-changes (2)`.
- **`apps/fabro-web/app/lib/run-events.test.tsx`**: SSE envelope with
  `stage_id: "verify@2"` triggers invalidation of
  `stageTurns(runId, "verify@2")`.
- **Graph test** (`apps/fabro-web/app/routes/run-overview.test.tsx` or
  similar): two visits of the same node — graph status follows the
  cascade, click target is the latest visit.

## Out of scope

- Wiring up the production `/runs/{id}/stages/{stageId}/turns` handler
  (`lib/crates/fabro-server/src/server/handler/mod.rs:116` is
  `not_implemented`). The events fallback is doing the work today and will
  keep doing it; the per-stage filter fix is what unblocks multi-visit
  display.
- Per-visit billing breakdown in `get_run_billing` — that path still uses
  the existing per-node duration map.

## Critical files

- `docs/public/api-reference/fabro-api.yaml` — schema source of truth
- `lib/crates/fabro-server/src/server/handler/billing.rs` — `list_run_stages`
- `lib/crates/fabro-types/src/run_projection.rs` — `iter_stages` data source
- `lib/crates/fabro-workflow/src/lib.rs` — new duration extractor
- `lib/crates/fabro-server/src/demo/mod.rs` — fixture
- `apps/fabro-web/app/routes/run-stages.tsx` — events filter + header
- `apps/fabro-web/app/components/stage-sidebar.tsx` — display label
- `apps/fabro-web/app/lib/stage-sidebar.ts` — visibility filter + mapping
- `apps/fabro-web/app/lib/run-events.ts` — SSE cache invalidation
- `apps/fabro-web/app/routes/run-overview.tsx` — graph aggregation policy
- `lib/crates/fabro-types/src/outcome.rs` — `From<StageOutcome> for StageState` (already exists; reuse)

## Verification

Build:
- `cargo build -p fabro-api` — regenerates types from updated YAML
- `cd lib/packages/fabro-api-client && bun run generate`
- `cargo build --workspace`

Tests:
- `cargo nextest run -p fabro-server` — conformance + new tests in
  `lib/crates/fabro-server/src/server/tests.rs` (distinguish_visits,
  shows_retrying_after_failed_event, shows_retrying_when_failed_will_retry)
- `cd apps/fabro-web && bun test && bun run typecheck`

End-to-end (single-visit regression):
- `fabro server start` → open the demo URL → confirm
  `detect-drift`/`propose-changes`/`review-changes` show no `(N)` suffix.
  URLs are `.../stages/detect-drift@1` etc. Graph still highlights correctly.

End-to-end (the fix):
- Demo: two `apply-changes` rows. Sidebar shows `apply-changes` and
  `apply-changes (2)`. URLs `.../stages/apply-changes@1` vs
  `.../stages/apply-changes@2` are distinct and load distinct content. Graph
  lights up the same `apply` node either way.
- Real loop run: trigger a workflow that loops `verify` (fail → fix → pass).
  Confirm two distinct entries with distinct statuses, durations, turns, and
  command logs (`/stages/verify@1/logs/stdout` vs
  `/stages/verify@2/logs/stdout`).

API contract:
- `curl /api/v1/runs/{id}/stages | jq` — `id` contains `@`; `visit` is
  present and ≥ 1; `node_id` is the bare node id with no `@`. The old
  `dot_id` field is gone.

Negative checks:
- Terminal run: no trailing in-flight row.
- Parallel fanout: still one row per group (parallel branches don't promote
  to separate `RunStage` entries).
- Empty checkpoint: empty list, no panic.
- **Retry mid-flight**: trigger a stage that fails and then retries; sidebar
  shows `Retrying`, not `Failed`. Confirms P1 regression guard.
- **SSE liveness**: while a run is active and a stage emits events, the
  selected stage's turn list updates without a manual refresh — confirms
  cache invalidation works against suffixed keys.


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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.