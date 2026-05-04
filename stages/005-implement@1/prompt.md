Goal: # Per-Stage Events Endpoint

## Context

The stage detail page at `/runs/{id}/stages/{stageId}` renders an empty right pane for stages whose events fall past the first 1000 events of a run (e.g. `fmt`, `fixup` after a long `implement` + `simplify_*` chain).

Two architectural problems compound:

1. `/runs/{id}/stages/{stageId}/turns` is wired to `not_implemented` (501) in real mode (`lib/crates/fabro-server/src/server/handler/mod.rs:116`). The frontend treats 501 as `null` (`apps/fabro-web/app/lib/api-client.ts:43`) and falls back to events.
2. The events fallback fetches the run-wide `/runs/{id}/events?limit=1000` (oldest-first, capped at 1000 per `lib/crates/fabro-server/src/server/handler/events.rs:35`). For a 43-minute run with chatty agent stages early in the timeline, later stages are stranded past the cap. `turnsFromEvents` filters by `node_id`, finds nothing, and renders an empty body.

`StageTurn` is also a presentation-shaped wire schema that only models LLM kinds (`system | assistant | tool`), not commands — so even a real implementation of `/turns` would not serve shell stages without schema growth.

The intended outcome: stage detail renders correctly for every stage, scales to long stages, and removes the dual-source data path. We collapse to one concept on the wire — events, scoped to a single stage. The cross-tab SSE coordinator (already merged: `apps/fabro-web/app/lib/cross-tab-sse.ts`, `run-events.ts`, `board-events.ts`) provides liveness via cache invalidation; the new endpoint is its canonical-data counterpart.

## Approach

Replace `/runs/{id}/stages/{stageId}/turns` with `GET /runs/{id}/stages/{stageId}/events?since_seq=&limit=`. Same shape as `/runs/{id}/events`, scoped server-side to events whose `node_id` matches the path parameter. Delete the `StageTurn` schema family entirely. The frontend keeps its existing `TurnType` discriminated union as a *local* presentation type built from events.

The frontend stage detail page becomes single-source: fetch `/stages/{stageId}/events` (paginating from `since_seq=1` via cursor until `meta.has_more === false`), feed the array into the existing `turnsFromEvents` reducer, render. Live updates require two coordinated frontend changes — neither is "free":

1. Swap `runs.stageTurns` → `runs.stageEvents` in `queryKeysForRunEvent`.
2. **Expand `queryKeysForRunEvent`'s coverage** to include every event type the reducer reads. Today it only handles `stage.{started,completed,failed}` and `command.{started,completed}`; the reducer also reads `stage.prompt`, `agent.message`, `agent.tool.started`, `agent.tool.completed`, all of which currently return `[]` from the invalidation map and silently fail to refresh agent activity mid-run. Add a `STAGE_ACTIVITY_EVENTS` set covering all six and route them to `runs.stageEvents(runId, stageId)` invalidations.

`run-detail.tsx` already calls `useRunEvents(runId)`, so once the invalidation map is correct, the stage page receives liveness without its own subscription.

## Server changes

### 1. Add `node_id` filter to the events store

`lib/crates/fabro-store/src/slate/run_store.rs:205` — `list_events_from_with_limit` currently filters only by `seq`. Add a sibling that takes a node id:

```rust
pub async fn list_events_for_node_from_with_limit(
    &self,
    node_id: &str,
    start_seq: u32,
    limit: usize,
) -> Result<Vec<EventEnvelope>>
```

**Implementation order matters.** Do **not** call the existing `list_events_from_with_limit` and filter the result — that helper truncates at `limit + 1` before any node filter, so for stages with sparse events you would silently drop matches. The correct order is:

1. Scan the run-events prefix from `start_seq` upward (unbounded inner scan, mirroring lines 314-335 in `run_store.rs`).
2. For each envelope, keep only those where `event.node_id.as_deref() == Some(node_id)`.
3. Take the first `limit + 1` matches and return them; the handler computes `has_more` from the +1.

`EventEnvelope::event::node_id` is already exposed (`lib/crates/fabro-types/src/run_event/mod.rs:34`).

Performance note (acknowledged, not optimized in v1): for a stage whose events are sparse late in a long run's event log, this scans the full tail. A `node_id`-keyed secondary index is a future optimization — flag if profiling shows it matters.

Add unit tests covering: events for the requested node are returned in seq order; events for other nodes are skipped; events with `node_id = None` are skipped; pagination via `start_seq` works on the filtered slice; **a node with sparse matches preceded by many unrelated events still returns its full slice (no premature truncation)**.

### 2. Add a stage-scoped extractor

`lib/crates/fabro-server/src/principal_middleware.rs` — `RequireRunScoped` extracts `Path<String>` (line 178), which will fail on a two-param route. Add `RequireRunStageScoped(RunId, String)` modeled on the existing `RequireRunBlob` (lines 188-199), which already handles two-param paths:

```rust
pub(crate) struct RequireRunStageScoped(pub(crate) RunId, pub(crate) String);

impl FromRequestParts<Arc<AppState>> for RequireRunStageScoped {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let Path((id, stage_id)): Path<(String, String)> = Path::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let run_id = parse_run_id_path(&id)?;
        require_worker_or_user_for_run(&auth_slot_from_parts(parts), &run_id)
            .map_err(IntoResponse::into_response)?;
        Ok(Self(run_id, stage_id))
    }
}
```

Visibility (`pub(crate)` on struct and fields) matches every existing extractor at `principal_middleware.rs:52-56`. Do **not** use `pub` — it would widen the server crate's API surface without need.

**Wire the new extractor through the server module.** Handlers reach extractors via `super::super::` re-exports from `server.rs` (e.g. `events.rs:3-9`). Add `RequireRunStageScoped` to the existing re-export bundle at `lib/crates/fabro-server/src/server.rs:126-129`:

```rust
use crate::principal_middleware::{
    AuthContextSlot, RequestAuth, RequestAuthContext, RequireRunBlob, RequireRunScoped,
    RequireRunStageScoped, RequireStageArtifact, RequiredUser, principal_middleware,
};
```

Without this, `events.rs` cannot reference the new extractor through `super::super::`.

### 3. Add the per-stage events route

`lib/crates/fabro-server/src/server/handler/events.rs` — alongside `list_run_events` (lines 174-201), add `list_run_stage_events`:

```rust
async fn list_run_stage_events(
    RequireRunStageScoped(id, stage_id): RequireRunStageScoped,
    State(state): State<Arc<AppState>>,
    Query(params): Query<EventListParams>,
) -> Response { ... }
```

Reuse the existing `EventListParams` (since_seq + limit, default 100, max 1000). Wrap the response in `PaginatedEventList { data, meta: PaginationMeta { has_more } }` exactly as `list_run_events` does. Register the route in `events::routes()` (events.rs:11):

```rust
.route("/runs/{id}/stages/{stageId}/events", get(list_run_stage_events))
```

**Unknown-stage contract:** when the run exists but `stageId` matches no events, return `200 { data: [], meta: { has_more: false } }`. This is a filtered event-log view, not a stage-metadata lookup; emptiness ≠ not-found. When the *run* doesn't exist, the existing `events.rs:199` pattern still applies — return 404. In OpenAPI: keep the 404 response on the new path; change its description to `"Run not found."` (not "Run or stage not found"). Assert both behaviors in the handler test.

The `stageId` path param is `node_id` (matches the existing URL convention; `RunStage.id == node_id` per `lib/crates/fabro-server/src/server/handler/billing.rs:101-107`). No visit disambiguation — that mirrors today's behavior. *Out of scope:* the pre-existing UX issue where a node visited twice (e.g. `verify` after `simplify_gpt` and again after `fixup`) collapses to one `node_id` in the sidebar; both visits' events would be returned together. Document but do not fix here.

### 4. Remove the turns route, schema, and demo fixture

- `lib/crates/fabro-server/src/server/handler/mod.rs:60-62, 116` — remove the demo and real `/turns` route registrations.
- `lib/crates/fabro-server/src/demo/mod.rs:136-143` — remove `get_stage_turns`. Remove `runs::turns()` fixture (lines 1215-1228).
- `docs/public/api-reference/fabro-api.yaml`:
  - Delete path `/runs/{id}/stages/{stageId}/turns` (lines 1909-1936).
  - Delete schemas `StageTurn` (6378-6389), `SystemStageTurn` (6391-6404), `AssistantStageTurn` (6406-6419), `ToolStageTurn` (6421-6438), `PaginatedStageTurnList` (3859-3871). Verify no other path references them — `ToolUse` (6343-6376) is also referenced inside `ToolStageTurn`; check whether anything else uses it before deleting (the events stream carries tool data via `RunEvent.properties`, not via `ToolUse`, so it likely also goes).
  - Add path `/runs/{id}/stages/{stageId}/events` modeled after `/runs/{id}/events` (lines 1603-1667). Use existing `SinceSeq` + `EventLimit` parameters and `PaginatedEventList` response. **Do not reuse the existing `StageId` parameter** (lines 2925-2932) — it documents `node_id@visit` with example `code@2` and is genuinely needed in that form by command-logs/artifacts paths (`principal_middleware.rs:236`). Add a new parameter:
    ```yaml
    StageNodeId:
      name: stageId
      in: path
      required: true
      description: Workflow node id (matches RunStage.id; not visit-qualified).
      schema:
        type: string
      example: detect-drift
    ```
    Reference this new parameter on the events path; leave the existing `StageId` parameter in place for the other paths that legitimately use `node_id@visit`.

### 5. Add a demo stage events fixture

`lib/crates/fabro-server/src/server/handler/mod.rs:60` and `demo/mod.rs` — add `demo::get_stage_events` that returns `PaginatedEventList`. Hand-write ~7 `EventEnvelope`s for the existing `detect-drift` demo stage that recreate the content currently in `runs::turns()`:

- `stage.prompt` (system prompt text)
- `agent.message` (intro)
- `agent.tool.started` + `agent.tool.completed` × 2 (tool calls)
- `agent.message` (closing analysis)

Each with `node_id: Some("detect-drift")`, ascending `seq`, and `properties` matching the shape `turnsFromEvents` already reads (`text`, `tool_call_id`, `tool_name`, `arguments`, `output`, `is_error`).

**Do not reuse the existing `paginated_response` helper** (`demo/mod.rs:28`) — it takes `PaginationParams` (offset-based, `page[limit]/page[offset]`) and would silently ignore `since_seq`/`limit` from the events endpoint. Instead, give `demo::get_stage_events` its own params. Either share the real-mode `EventListParams` (preferred, single source of truth) or define a small demo-local equivalent. The handler body should:

1. Read `since_seq` (default 1, min 1) and `limit` (default 100, max 1000) from query.
2. Filter the fixture. `EventEnvelope { seq: u32, event: RunEvent }` (per `event_envelope.rs:5-10`), and `node_id: Option<String>` lives on the inner `event`, so the predicate is:
   ```rust
   envelope.seq >= since_seq
       && envelope.event.node_id.as_deref() == Some(stage_id.as_str())
   ```
   Use `.as_deref()` / `.as_str()` to avoid moving `stage_id` into the iterator closure.
3. Take the first `limit + 1` matches; set `has_more = matches.len() > limit`; truncate to `limit`.
4. Return `PaginatedEventList { data, meta: PaginationMeta { has_more } }`.

The cursor-pagination test in step 6 directly exercises this path.

### 6. Update and add integration tests

**Remove `listStageTurns` from the generic offset-pagination matrix.** `lib/crates/fabro-server/tests/it/pagination.rs:60-62` uses `?page[limit]=` (lines 82, 91). Events use `?since_seq=&limit=` — co-mingling them passes the shape assertion only because the limit param is silently ignored. Delete the `listStageTurns` entry from the `ENDPOINTS` array; do **not** replace it with `listStageEvents` in the same matrix.

**Add a cursor-pagination test for the demo stage-events endpoint.** New test (file: `lib/crates/fabro-server/tests/it/event_pagination.rs` or appended to the existing IT module) that exercises the demo `/runs/run-1/stages/detect-drift/events` endpoint with:
- `?limit=1` → `data.len() == 1`, `meta.has_more == true`.
- `?since_seq=N` → only events with `seq >= N` (where N is a known mid-fixture seq).
- No params → default `limit=100`, returns all 7 fixture events, `has_more == false`.

Do **not** repeat these assertions against `/runs/run-1/events` in demo mode — that route is wired to `not_implemented` (`mod.rs:38`) and would 501. Cursor pagination on the run-wide endpoint is already covered by `list_run_events` real-mode tests; cursor semantics for the stage endpoint are covered here.

**Add an HTTP handler test for the real-mode endpoint** (in `lib/crates/fabro-server/src/server/handler/events.rs` `#[cfg(test)]` block, or a dedicated `tests/it/stage_events.rs`):
- Seed a run-event store with a mix of envelopes: some with `node_id = Some("alpha")`, some with `node_id = Some("beta")`, some with `node_id = None`. Interleave seqs and include sparse `alpha` events past seq 100 to prove the scan walks past unrelated events.
- `GET /runs/{id}/stages/alpha/events` → only `alpha` events, in seq order.
- `GET /runs/{id}/stages/alpha/events?since_seq=K` → only `alpha` events with `seq >= K`.
- `GET /runs/{id}/stages/alpha/events?limit=1` → exactly one envelope, `has_more == true`.
- `GET /runs/{id}/stages/unknown-stage/events` (run exists, stage doesn't) → `200 { data: [], meta: { has_more: false } }`.
- `GET /runs/{absent_but_valid_id}/stages/alpha/events` → `404` with `"Run not found."` body, where `absent_but_valid_id` is a syntactically valid `RunId` (ULID-shaped) that simply isn't in the test store. Do not use a malformed string like `"nonexistent-run"` — `parse_run_id_path` (`server.rs:1668-1670`) would 400 before the handler runs, which tests the wrong path. If you also want to assert the 400 path, add a separate explicit test for it.
- Auth path coverage: a request without the appropriate run-scope auth returns 401/403, proving the new `RequireRunStageScoped` extractor enforces the same scope as `RequireRunScoped`.

### 7. Regenerate Rust API types

`lib/crates/fabro-api/build.rs` — `EventEnvelope` is already replaced with `fabro_types::EventEnvelope` (line 367); no new `with_replacement` calls needed. `cargo build -p fabro-api` will regenerate the reqwest client and progenitor types after the YAML edits.

## Frontend changes

### 1. Replace the query key

`apps/fabro-web/app/lib/query-keys.ts:47-48` — remove `runs.stageTurns`. Add:

```ts
stageEvents: (id: string, stageId: string, sinceSeq?: number, limit?: number) =>
  withQuery(
    `/api/v1/runs/${pathSegment(id)}/stages/${pathSegment(stageId)}/events`,
    { since_seq: sinceSeq, limit },
  ),
```

The base key (no params) is the SWR cache key for "all events for this stage."

### 2. Add a paginated stage-events hook

`apps/fabro-web/app/lib/queries.ts` — remove `useRunStageTurns` (lines 144-153). Add:

```ts
export function useRunStageEvents(id: string | undefined, stageId: string | undefined) {
  return useSWR<EventEnvelope[]>(
    id && stageId ? queryKeys.runs.stageEvents(id, stageId) : null,
    fetchAllStageEvents,
  );
}
```

`fetchAllStageEvents` is a cursor-paginated loop modeled on `apiPaginatedFetcher` (`api-client.ts:166-218`) but using `since_seq` instead of `page[offset]`:

- Start at `since_seq = 1`, `limit = 1000`.
- Each page yields `EventEnvelope[]` and `meta.has_more`.
- Append, set next `since_seq = highestSeq + 1`, loop until `!has_more` or safety cap (50 pages × 1000 = 50k events).
- **Empty-page guard** (matching `api-client.ts:193`): if `page.data.length === 0`, exit the loop with the accumulated events. A page with `has_more: true` but no data would otherwise spin until the safety cap. Treat it as a server invariant violation: log a `console.warn` and return what we have. This protects against fixture/server bugs without masking them — the warn surfaces the violation while keeping the UI stable.
- Return the flattened `EventEnvelope[]`.

This sits in `app/lib/api-client.ts` as `fetchAllStageEvents(key)` parsing `since_seq` out of the URL it's handed, mirroring how `apiPaginatedFetcher` is used today.

### 3. Refetch policy on invalidation

When `useRunEvents` invalidates the stage-events SWR key, SWR refetches via `fetchAllStageEvents`, which loops from `since_seq=1`. That is correct but potentially wasteful for long stages. Acceptable v1: the events list is bounded by stage size, not run size, and most stages have <1k events. If profiling shows otherwise, switch to a custom hook that holds the events array in component state and tail-fetches from `highestSeqSeen + 1` on invalidation. Not part of this plan.

### 4. Update the stage detail page

`apps/fabro-web/app/routes/run-stages.tsx`:

- Remove the `useRunStageTurns` import and call (lines 43, 608).
- Remove the `useRunEventsList` fallback wiring (lines 609-616).
- Remove `mapTurns` (lines 176-189) and `mapApiStageTurn` (lines 156-174). Drop the `ApiStageTurn`, `PaginatedStageTurnList`, `PaginatedEventList` imports (lines 50-52).
- Replace the dual-source flow. Note that the existing early return at `run-stages.tsx:619` (`if (!id || !stages.length) return EmptyState`) guarantees `selectedStage` is defined at this call site, so `selectedStage.id` (no `?.`) typechecks cleanly against the existing reducer signature `(events: EventEnvelope[], stageId: string)`:
  ```ts
  const stageEventsQuery = useRunStageEvents(id, selectedStage?.id);
  const turns = useMemo(
    () => selectedStage
      ? eventsToActivity(stageEventsQuery.data ?? [], selectedStage.id)
      : [],
    [stageEventsQuery.data, selectedStage],
  );
  ```
  The `selectedStage` ternary keeps the `useMemo` body type-safe even though the runtime path always has `selectedStage` defined; do not change the reducer signature.
- Rename `turnsFromEvents` → `eventsToActivity` (line 71). It still filters `e.node_id === stageId` (defensive, since the server already scoped) and produces the same `TurnType[]`. Keep the existing event handling for `stage.prompt`, `agent.message`, `agent.tool.*`, `command.*`. Keep the `TurnType` union as-is (lines 57-61) — it's purely local now.

### 5. Wire stage-events into cross-tab invalidation

`apps/fabro-web/app/lib/run-events.ts:54-110` (`queryKeysForRunEvent`) — the existing branches handle only `STAGE_EVENTS` (`stage.started/completed/failed`) and `COMMAND_EVENTS` (`command.started/completed`). The `eventsToActivity` reducer also reads `stage.prompt`, `agent.message`, `agent.tool.started`, `agent.tool.completed` — events for which `queryKeysForRunEvent` currently returns `[]`, meaning agent-stage activity does not refresh live.

Add a `STAGE_ACTIVITY_EVENTS` set covering every event type the reducer consumes:

```ts
const STAGE_ACTIVITY_EVENTS = new Set([
  "stage.prompt",
  "agent.message",
  "agent.tool.started",
  "agent.tool.completed",
  "command.started",
  "command.completed",
]);
```

For these (when the payload has a `node_id`), invalidate `queryKeys.runs.stageEvents(runId, stageId)`. The existing `STAGE_EVENTS` branch (lifecycle: `stage.started/completed/failed`) keeps its broader run-scoped invalidations (`stages`, `events`, `graph`, `detail`) and additionally invalidates `stageEvents(runId, stageId)` instead of `stageTurns`. The existing `COMMAND_EVENTS` branch is subsumed by `STAGE_ACTIVITY_EVENTS` — fold it in or keep separate, but ensure it invalidates `stageEvents` (not `stageTurns`).

No new subscription is needed: `run-detail.tsx:119` already calls `useRunEvents(params.id)`, and that subscription dispatches to per-stage keys via `queryKeysForRunEvent`. The stage detail page is a passive consumer — when any reducer-relevant event for its `node_id` arrives in any tab, SWR invalidates `runs.stageEvents(runId, stageId)`, the page refetches, the reducer rebuilds.

`apps/fabro-web/app/lib/run-events.ts:162-173` — `resyncKeysForRun` resyncs run-scoped keys on leader change. The stage-events key is per-stage, so it's not naturally in this list. Acceptable: on leader change SWR's existing focus/reconnect revalidation will refresh active stage-events keys. If gap recovery becomes a problem, add `runs.stageEvents(runId, currentStageId)` here, but the page can also just call `mutate` on its own key on visibility return. Not part of this plan.

**Tests for the live-invalidation path** (extend `apps/fabro-web/app/lib/query-keys.test.ts`):
- `stage.prompt` with `node_id` → invalidations include `runs.stageEvents(runId, nodeId)`.
- `agent.message` with `node_id` → invalidations include `runs.stageEvents(runId, nodeId)`.
- `agent.tool.completed` with `node_id` → invalidations include `runs.stageEvents(runId, nodeId)`.
- `command.completed` with `node_id` → invalidations include `runs.stageEvents(runId, nodeId)`.
- `stage.completed` (lifecycle) still invalidates the run-scoped keys plus `runs.stageEvents`.

### 6. Remove obsolete imports and tests

- `apps/fabro-web/app/lib/queries.ts:2-18` — drop `PaginatedStageTurnList` from the import list.
- `apps/fabro-web/app/lib/query-keys.test.ts:25-29` — replace the assertion that `stage.completed` invalidates `runs.stageTurns` with `runs.stageEvents`.
- `apps/fabro-web/app/lib/run-events.test.tsx` — search for `stageTurns`; update to `stageEvents`.

### 7. Regenerate the TS client (with explicit cleanup)

The generate script (`lib/packages/fabro-api-client/package.json:7`) writes `-o src` without a clean step — `openapi-generator-cli` writes file-by-file based on the schema list, so deleted schemas leave **stale model files behind** that remain importable. Steps:

1. From `lib/packages/fabro-api-client/`: `rm -rf src/models src/api` to drop all generated models and API surface.
2. `bun run generate` to repopulate from the updated YAML.
3. Verify no stale references remain: `rg "StageTurn|SystemStageTurn|AssistantStageTurn|ToolStageTurn|PaginatedStageTurnList|listStageTurns" lib/packages/fabro-api-client apps/fabro-web` should return no matches.
4. `cd apps/fabro-web && bun run typecheck` to confirm the import graph stays consistent.

(Optional follow-up not in this plan: add a `prebuild` clean step to the package script so this doesn't trip future schema deletions.)

## Reused infrastructure

- `lib/crates/fabro-store` `list_events_from_with_limit` — the new method follows the same prefix-scan pattern.
- `lib/crates/fabro-server` `EventListParams`, `PaginatedEventList`, `PaginationMeta` — reused as-is. `RequireRunBlob` (lines 188-199) is the model for the new `RequireRunStageScoped` extractor.
- `apps/fabro-web/app/lib/cross-tab-sse.ts` `subscribeToCrossTabSse` — used implicitly via the existing `useRunEvents` plumbing in `run-events.ts`. No changes to the coordinator itself.
- `apps/fabro-web/app/routes/run-stages.tsx` `turnsFromEvents` reducer (renamed) — kept as the local presentation projection.
- `apps/fabro-web/app/lib/api-client.ts` `apiPaginatedFetcher` shape — `fetchAllStageEvents` mirrors its safety caps.

## Out of scope

- Same-`node_id` repeat visits (e.g. two `verify` rows in the sidebar pointing at the same URL). Pre-existing; needs URL design (`/stages/{nodeId}/{visit}` or similar) and `RunStage.id` disambiguation.
- Tail-fetch optimization for the SWR invalidation path (refetch from `since_seq=highestSeen+1` instead of full reload). Defer to first profiling signal.
- Any `/api/v1/attach` server-side replay or schema changes — explicitly excluded by the cross-tab SSE plan.

## Verification

Test commands:

- `cargo nextest run -p fabro-store` — confirms the new `list_events_for_node_from_with_limit` filter, including the sparse-stage scan-then-filter case.
- `cargo nextest run -p fabro-server` — runs the conformance test (`server::tests` + `it/pagination.rs`), the new cursor-pagination test, and the new stage-events handler test (mixed `node_id`s, `since_seq`, `limit`, unknown-stage 200, auth extractor).
- `cd apps/fabro-web && bun run typecheck` — must pass after the generated TS client is regenerated and obsolete imports are removed. Will fail loudly if stale `StageTurn`-related files were left behind.
- `cd apps/fabro-web && bun test` — runs `query-keys.test.ts` (now includes the new invalidation cases for `stage.prompt`, `agent.message`, `agent.tool.completed`), `run-events.test.tsx`, `board-events.test.tsx`.
- Add `run-stages.test.ts` cases (currently only covers `isSafeMarkdownHref`) for `eventsToActivity`: given a sequence of `command.started` + `command.completed` events for `node_id="fmt"`, return one `command` turn; given `agent.tool.started` + `agent.tool.completed`, return one `tool` turn; events for other `node_id`s are filtered out.

End-to-end manual check:

1. `fabro server start` (real mode), then in another terminal `cd apps/fabro-web && bun run dev`.
2. Reproduce the original bug URL — a finished run with a long `implement`-style stage followed by `fmt`/`fixup`. Confirm those stages now render their command panes (script + stdout/stderr).
3. Start a fresh run via the CLI; open its detail page mid-run. Watch a stage transition from running → succeeded; confirm the right pane updates without a manual reload (cross-tab invalidation).
4. Open two tabs on the same running run; confirm only one `/api/v1/attach` EventStream is active in DevTools and both tabs' stage panes update from the same shared stream.
5. Demo mode (`fabro server start` + `X-Fabro-Demo: 1` header via the toggle): open the `detect-drift` stage; confirm system prompt + assistant + tool turns still render from the new demo events fixture.


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