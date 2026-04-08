# Production Web UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the fabro web UI to the real fabro server via HTTP, retain a demo mode toggle, and remove UI for server features that are not implemented in real mode.

**Architecture:** The web UI already calls the fabro server's `/api/v1/` endpoints via `apiFetch`/`apiJson` helpers. Demo mode is toggled per-request via a `fabro-demo` cookie that the server middleware converts to an `X-Fabro-Demo: 1` header, dispatching to separate demo vs real route sets. The core work is: (1) fix the real `/boards/runs` handler to return the `RunListItem` shape the UI expects, (2) make the UI conditionally hide navigation and routes for features not implemented in real mode (workflows, insights, run files), (3) add a demo `/boards/runs` handler so demo mode also works through this endpoint, and (4) fix the demo `get_run_status` to return `StoreRunSummary` shape matching the OpenAPI spec so the run-detail loader works in both modes.

**Tech Stack:** Rust (Axum server), TypeScript (React 19 + React Router + Vite), Playwright (browser tests)

---

## Key architectural decisions

### Decision 1: Fix real `/boards/runs` to return `RunListItem` shape

The OpenAPI spec declares `/boards/runs` returns `PaginatedRunList` containing `RunListItem` objects. However, the real `list_board_runs` handler currently returns `RunStatusResponse` objects (id, status, error, queue_position, created_at). The UI's runs board, run-detail, and run-overview loaders all consume `/boards/runs` and expect `RunListItem` fields (repository, title, workflow, status as `BoardColumn`, pull_request, timings, sandbox, question).

**Decision:** Enrich the real `list_board_runs` handler to return `RunListItem`-shaped data by pulling `goal` (as title), `workflow_slug`/`workflow_name`, `host_repo_path` (as repository name), `duration_ms` (as timing), and `total_usd_micros` from `RunSummary`. Map `RunStatus` lifecycle values to `BoardColumn` values: `Running` -> `"working"`, `Paused` -> `"pending"`, `Completed` -> `"merge"`, everything else (`Submitted`, `Queued`, `Starting`, `Failed`, `Cancelled`) -> excluded from the board (they are not actionable board items).

**Justification:** This aligns the real handler with the OpenAPI spec and avoids bifurcating the UI's data layer into two incompatible response shapes. The store already has the needed fields. Fields not available from the store (pull_request, sandbox, checks, question) are left `null`/absent -- the UI already handles their optionality with `?.` chains.

**Impact on existing tests:** Several existing integration tests (e.g., `cancel_run_sets_status_reason`, `cancel_run_overwrites_pending_pause_request`, queue position tests) assert on `status_reason` and `pending_control` fields from `/boards/runs` responses. These fields exist on `RunStatusResponse` but not on `RunListItem`. These tests use `/boards/runs` as a secondary assertion to verify server state -- they should be updated to assert those fields via `/runs/{id}` (which returns `StoreRunSummary` containing both fields) instead, then assert the board-specific shape from `/boards/runs`.

### Decision 2: Add `/boards/runs` to demo routes

The demo routes currently have `/runs` but NOT `/boards/runs`. The UI exclusively calls `/boards/runs` for the runs board. Since the server dispatches to demo vs real routes based on the `X-Fabro-Demo` header, and both route sets need `/boards/runs`, add it to demo routes.

**Decision:** Add a `demo::list_board_runs` handler that returns the same `RunListItem` data the existing `demo::list_runs` returns, but under the `/boards/runs` path.

### Decision 3: Fix demo `get_run_status` to return `StoreRunSummary` shape

The OpenAPI spec says `GET /runs/{id}` returns `StoreRunSummary` (with fields `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`, etc.). The real handler correctly returns `RunSummary` (the Rust type that maps to `StoreRunSummary`). But the demo handler returns `RunStatusResponse` (with fields `id`, `status`, `error`, `queue_position`, `created_at`) -- a completely different shape that violates the spec.

**Decision:** Change `demo::get_run_status` to return `StoreRunSummary`-shaped JSON by constructing it from the matching `RunListItem` in the demo data. This makes the run-detail loader work identically in both demo and real modes.

### Decision 4: Conditionally hide unimplemented features based on demo mode

The real server returns `not_implemented` (501) for: `/workflows`, `/workflows/{name}`, `/workflows/{name}/runs`, `/insights/*`, `/runs/{id}/stages`, `/runs/{id}/stages/{stageId}/turns`, `/runs/{id}/settings`.

The "Files Changed" tab calls `/runs/{id}/files` which does not exist in either demo or real routes -- it has no server endpoint at all.

**Decision:** The `auth/me` response already includes `demoMode: boolean`. Use this flag in the UI to:
- Hide the "Workflows" and "Insights" nav items when not in demo mode
- Remove the "Stages" and "Settings" tabs from the run detail view when not in demo mode (keep Overview, Graph, Billing which all use real endpoints)
- Remove the "Files Changed" tab always (the endpoint does not exist in either mode)
- Redirect away from workflow/insight routes when not in demo mode

This avoids showing users broken pages. The routes remain in the router for demo mode.

**Justification:** Per user instruction: "for functionality that has been removed from fabro server, remove the corresponding UI from fabro web for now." Using `demoMode` from the existing auth response is the simplest mechanism -- no new API call needed.

### Decision 5: Run overview graceful degradation

The run-overview loader fetches both `/runs/{id}/stages` (501 in real mode) and `/boards/runs`. It also tries to fetch `/workflows/{name}` for the graph dot source.

**Decision:** Make the run-overview loader resilient: catch 501 errors from `/runs/{id}/stages` and return an empty stages list. Remove the `/boards/runs` and `/workflows/{name}` fetches entirely -- the overview doesn't need the workflow slug (it got it just to fetch the graph dot, but that's redundant with the Graph tab), and the graph dot source is only useful for Graphviz rendering which the Graph tab already handles.

### Decision 6: Run detail loader -- use `/runs/{id}` instead of searching `/boards/runs`

The run-detail loader currently fetches ALL board runs via `/boards/runs` and finds the run by ID. This is wasteful and won't scale.

**Decision:** Change run-detail loader to fetch `/runs/{id}` directly. The real handler returns `RunSummary` (which contains `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`). Map this to the same shape the component expects. After fixing the demo handler (Decision 3), the demo `/runs/{id}` also returns `StoreRunSummary`-shaped data, so the same mapping works in both modes.

### Decision 7: Propagate `demoMode` via React context

Currently `demoMode` is only available in the app-shell loader data. Child routes need it to conditionally render features.

**Decision:** The app-shell already passes `demoMode` from `getAuthMe()`. Add a `DemoModeProvider` React context so child components can access it via `useDemoMode()`.

### Decision 8: Workflow-definition uses hardcoded static data

`workflow-definition.tsx` imports `workflowData` from `workflow-detail.tsx` and reads from the static record by name, ignoring the loader data. This is a demo-only artifact.

**Decision:** Since workflows are demo-only for now, this is acceptable. No change needed -- the route is only accessible in demo mode.

---

## File structure

### Files to modify

- `lib/crates/fabro-server/src/server.rs` -- Enrich real `list_board_runs` to return `RunListItem` shape; no changes to routes
- `lib/crates/fabro-server/src/demo/mod.rs` -- Add `list_board_runs` handler reusing existing run data; fix `get_run_status` to return `StoreRunSummary` shape
- `apps/fabro-web/app/layouts/app-shell.tsx` -- Conditionally hide nav items based on `demoMode`; export demo mode via context
- `apps/fabro-web/app/lib/demo-mode.tsx` -- New file: `DemoModeProvider` context and `useDemoMode()` hook
- `apps/fabro-web/app/routes/runs.tsx` -- Use `/boards/runs` (already does); no structural changes needed
- `apps/fabro-web/app/routes/run-detail.tsx` -- Change loader to use `/runs/{id}` instead of searching `/boards/runs`; conditionally hide tabs; always hide "Files Changed"
- `apps/fabro-web/app/routes/run-overview.tsx` -- Make loader resilient to 501 from stages endpoint; remove `/boards/runs` dependency
- `apps/fabro-web/app/routes/run-stages.tsx` -- No loader changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/run-graph.tsx` -- Make loader resilient to 501 from stages endpoint; use `/runs/{id}/graph` (real, works)
- `apps/fabro-web/app/routes/run-settings.tsx` -- No loader changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/run-files.tsx` -- No loader changes; tab always hidden (endpoint doesn't exist)
- `apps/fabro-web/app/routes/run-billing.tsx` -- No changes; uses `/runs/{id}/billing` which is implemented in real mode
- `apps/fabro-web/app/routes/workflows.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/workflow-detail.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/insights.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/settings.tsx` -- No changes; uses `/settings` which is implemented in real mode
- `apps/fabro-web/app/data/runs.ts` -- Add `mapRunSummaryToRunItem()` for mapping `/runs/{id}` response
- `apps/fabro-web/app/api.ts` -- Add `apiJsonOrNull()` helper for graceful 501 handling

### Files to create

- `apps/fabro-web/app/lib/demo-mode.tsx` -- DemoModeProvider and useDemoMode hook
- `apps/fabro-web/tests/playwright.config.ts` -- Playwright configuration
- `apps/fabro-web/tests/browser/smoke.test.ts` -- Browser smoke tests

---

## Task 1: Add `/boards/runs` to demo routes

**Files:**
- Modify: `lib/crates/fabro-server/src/demo/mod.rs`
- Modify: `lib/crates/fabro-server/src/server.rs:847-923` (demo_routes function)

- [ ] **Step 1: Write failing test**

Add a Rust integration test that sends `GET /api/v1/boards/runs` with the `X-Fabro-Demo: 1` header and expects a 200 response with `data` array containing `RunListItem`-shaped objects (having `id`, `repository`, `title`, `workflow`, `status`, `created_at` fields).

```rust
// In lib/crates/fabro-server/src/server.rs tests section
#[tokio::test]
async fn demo_boards_runs_returns_run_list_items() {
    let state = create_app_state();
    let app = build_router(state, AuthMode::Disabled);
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/boards/runs")
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    let data = body["data"].as_array().expect("data should be array");
    assert!(!data.is_empty(), "demo should return runs");
    let first = &data[0];
    assert!(first["id"].is_string());
    assert!(first["repository"].is_object());
    assert!(first["title"].is_string());
    assert!(first["workflow"].is_object());
    assert!(first["status"].is_string());
    assert!(first["created_at"].is_string());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- demo_boards_runs_returns_run_list_items`
Expected: FAIL (404 or route not found because `/boards/runs` is not in demo routes)

- [ ] **Step 3: Implement demo `/boards/runs` handler**

In `demo/mod.rs`, add a `list_board_runs` function that delegates to the existing `list_runs` logic (which already returns `RunListItem`-shaped data):

```rust
pub(crate) async fn list_board_runs(
    auth: AuthenticatedService,
    state: State<Arc<AppState>>,
    pagination: Query<PaginationParams>,
) -> Response {
    list_runs(auth, state, pagination).await
}
```

In `server.rs` `demo_routes()`, add the route:

```rust
.route("/boards/runs", get(demo::list_board_runs))
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- demo_boards_runs_returns_run_list_items`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run full server test suite to check for regressions:
Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && ulimit -n 4096 && cargo nextest run -p fabro-server`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-server/src/demo/mod.rs lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): add /boards/runs to demo routes"
```

---

## Task 2: Fix demo `get_run_status` to return `StoreRunSummary` shape

**Files:**
- Modify: `lib/crates/fabro-server/src/demo/mod.rs:173-194` (get_run_status function)

The demo `get_run_status` currently returns `RunStatusResponse` (with `id`, `status`, `error`, `queue_position`, `created_at`). The OpenAPI spec says `GET /runs/{id}` returns `StoreRunSummary` (with `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`, etc.). The real handler already returns the correct shape. The demo must match so the UI can use a single mapping function.

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn demo_get_run_returns_store_run_summary_shape() {
    let state = create_app_state();
    let app = build_router(state, AuthMode::Disabled);
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/runs/run-1")
        .header("X-Fabro-Demo", "1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    // Should have StoreRunSummary fields, not RunStatusResponse fields
    assert!(body["run_id"].is_string(), "should have run_id field");
    assert!(body["goal"].is_string(), "should have goal field");
    assert!(body["workflow_slug"].is_string(), "should have workflow_slug field");
    // Should NOT have RunStatusResponse-only fields
    assert!(body["queue_position"].is_null(), "should not have queue_position");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- demo_get_run_returns_store_run_summary_shape`
Expected: FAIL (currently returns `id` not `run_id`, has `queue_position`, lacks `goal`/`workflow_slug`)

- [ ] **Step 3: Rewrite demo `get_run_status` to return `StoreRunSummary` shape**

Replace the handler body to construct a `StoreRunSummary`-shaped JSON response from the matching `RunListItem`:

```rust
pub(crate) async fn get_run_status(
    _auth: AuthenticatedService,
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match runs::list_items().into_iter().find(|r| r.id == id) {
        Some(item) => {
            let elapsed_ms = item.timings.as_ref().map(|t| (t.elapsed_secs * 1000.0) as u64);
            (
                StatusCode::OK,
                Json(json!({
                    "run_id": item.id,
                    "goal": item.title,
                    "workflow_slug": item.workflow.slug,
                    "workflow_name": item.workflow.slug,
                    "host_repo_path": format!("/demo/{}", item.repository.name),
                    "labels": {},
                    "start_time": item.created_at.to_rfc3339(),
                    "status": "running",
                    "status_reason": null,
                    "pending_control": null,
                    "duration_ms": elapsed_ms,
                    "total_usd_micros": null,
                })),
            )
                .into_response()
        }
        None => ApiError::not_found("Run not found.").into_response(),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- demo_get_run_returns_store_run_summary_shape`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run full server test suite:
Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && ulimit -n 4096 && cargo nextest run -p fabro-server`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-server/src/demo/mod.rs
git commit -m "fix(server): demo get_run_status returns StoreRunSummary shape matching OpenAPI spec"
```

---

## Task 3: Enrich real `/boards/runs` to return `RunListItem` shape

**Files:**
- Modify: `lib/crates/fabro-server/src/server.rs:2017-2083` (list_board_runs function)

- [ ] **Step 1: Write failing test**

Add a Rust integration test that creates a run, starts it, then calls `GET /api/v1/boards/runs` (without demo header) and expects `RunListItem`-shaped objects with `repository`, `title`, `workflow`, and `status` as a `BoardColumn` value.

```rust
#[tokio::test]
async fn boards_runs_returns_run_list_items_with_board_columns() {
    let state = create_app_state();
    let app = build_router(Arc::clone(&state), AuthMode::Disabled);
    let run_id = create_and_start_run(&app, MINIMAL_DOT).await;

    // Set run to running so it appears on the board
    {
        let id = run_id.parse::<RunId>().unwrap();
        let mut runs = state.runs.lock().expect("runs lock poisoned");
        let managed_run = runs.get_mut(&id).expect("run should exist");
        managed_run.status = RunStatus::Running;
    }

    let req = Request::builder()
        .method("GET")
        .uri(api("/boards/runs"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response.into_body()).await;
    let data = body["data"].as_array().expect("data should be array");
    let item = data.iter()
        .find(|i| i["id"].as_str() == Some(&run_id))
        .expect("run should be in board");
    // Should have RunListItem fields
    assert!(item["title"].is_string());
    assert!(item["repository"].is_object());
    assert!(item["workflow"].is_object());
    // Status should be a board column, not a lifecycle status
    let status = item["status"].as_str().unwrap();
    assert!(
        ["working", "pending", "review", "merge"].contains(&status),
        "status should be a board column, got: {status}"
    );
    assert!(item["created_at"].is_string());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- boards_runs_returns_run_list_items_with_board_columns`
Expected: FAIL (current handler returns RunStatusResponse shape without title/repository/workflow, and status is lifecycle not board column)

- [ ] **Step 3: Rewrite `list_board_runs` to return enriched `RunListItem` data**

Replace the `list_board_runs` handler body with logic that:
1. Collects live run data from `state.runs` (id, status, created_at)
2. Fetches `RunSummary` data from `state.store.list_runs()`
3. Maps `RunStatus` to `BoardColumn`:
   - `Running` -> `"working"`
   - `Paused` -> `"pending"`
   - `Completed` -> `"merge"`
   - All others (`Submitted`, `Queued`, `Starting`, `Failed`, `Cancelled`) -> excluded from board
4. Constructs `RunListItem`-shaped JSON for each included run:

```rust
async fn list_board_runs(
    _auth: AuthenticatedService,
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationParams>,
) -> Response {
    let live_runs: HashMap<RunId, (RunStatus, DateTime<Utc>)> = {
        let runs = state.runs.lock().expect("runs lock poisoned");
        runs.iter()
            .map(|(id, mr)| (*id, (mr.status, mr.created_at)))
            .collect()
    };
    let summaries = match state
        .store
        .list_runs(&fabro_store::ListRunsQuery::default())
        .await
    {
        Ok(runs) => runs
            .into_iter()
            .map(|s| (s.run_id, s))
            .collect::<HashMap<_, _>>(),
        Err(err) => {
            return ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                .into_response();
        }
    };

    fn board_column(status: RunStatus) -> Option<&'static str> {
        match status {
            RunStatus::Running => Some("working"),
            RunStatus::Paused => Some("pending"),
            RunStatus::Completed => Some("merge"),
            _ => None,
        }
    }

    let all_items: Vec<serde_json::Value> = live_runs
        .iter()
        .filter_map(|(id, (status, created_at))| {
            let column = board_column(*status)?;
            let summary = summaries.get(id);
            let title = summary
                .and_then(|s| s.goal.as_deref())
                .unwrap_or("Untitled run");
            let workflow_slug = summary
                .and_then(|s| s.workflow_slug.as_deref())
                .unwrap_or("unknown");
            let workflow_name = summary
                .and_then(|s| s.workflow_name.as_deref())
                .unwrap_or(workflow_slug);
            let repo_name = summary
                .and_then(|s| s.host_repo_path.as_deref())
                .and_then(|p| p.rsplit('/').next())
                .unwrap_or("unknown");
            let elapsed_secs = summary
                .and_then(|s| s.duration_ms)
                .map(|ms| ms as f64 / 1000.0);
            Some(json!({
                "id": id.to_string(),
                "title": title,
                "repository": { "name": repo_name },
                "workflow": { "slug": workflow_slug, "name": workflow_name },
                "status": column,
                "created_at": created_at.to_rfc3339(),
                "timings": elapsed_secs.map(|s| json!({ "elapsed_secs": s })),
            }))
        })
        .collect();

    let limit = pagination.limit.clamp(1, 100) as usize;
    let offset = pagination.offset as usize;
    let page: Vec<_> = all_items.into_iter().skip(offset).take(limit + 1).collect();
    let has_more = page.len() > limit;
    let data: Vec<_> = page.into_iter().take(limit).collect();
    (
        StatusCode::OK,
        Json(json!({ "data": data, "meta": { "has_more": has_more } })),
    )
        .into_response()
}
```

Note: the exact types and imports will need to be adjusted based on what is in scope. The handler already has access to `HashMap`, `RunId`, etc. from the existing module scope.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && cargo nextest run -p fabro-server -- boards_runs_returns_run_list_items_with_board_columns`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Some existing tests assert on `status_reason` and `pending_control` fields from `/boards/runs` responses (e.g., tests for cancel and pause flows). These fields no longer exist in the `RunListItem` shape. Update those tests to assert `status_reason`/`pending_control` via `GET /runs/{id}` (which returns `StoreRunSummary` containing both fields) instead. The `/boards/runs` assertions in those tests should be updated to check for the new `RunListItem` fields or removed if redundant.

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && ulimit -n 4096 && cargo nextest run -p fabro-server`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): enrich /boards/runs to return RunListItem shape with board columns"
```

---

## Task 4: Fix run-detail loader to use `/runs/{id}` directly

**Files:**
- Modify: `apps/fabro-web/app/routes/run-detail.tsx:19-30`
- Modify: `apps/fabro-web/app/data/runs.ts`

- [ ] **Step 1: Write failing test**

Add a TypeScript test in `apps/fabro-web/app/data/runs.test.ts` that tests a new `mapRunSummaryToRunItem()` function which maps the `/runs/{id}` response shape (a `StoreRunSummary` with `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`) to the `RunItem` shape.

```typescript
import { describe, expect, test } from "bun:test";
import { mapRunSummaryToRunItem } from "./runs";

describe("mapRunSummaryToRunItem", () => {
  test("maps store run summary to RunItem", () => {
    const summary = {
      run_id: "01ABC",
      goal: "Fix the build",
      workflow_slug: "fix_build",
      workflow_name: "Fix Build",
      host_repo_path: "/home/user/myrepo",
      status: "running",
      duration_ms: 65000,
      total_usd_micros: 500000,
      labels: {},
      start_time: "2026-04-08T12:00:00Z",
      status_reason: null,
      pending_control: null,
    };
    const item = mapRunSummaryToRunItem(summary);
    expect(item.id).toBe("01ABC");
    expect(item.title).toBe("Fix the build");
    expect(item.workflow).toBe("fix_build");
    expect(item.repo).toBe("myrepo");
    expect(item.elapsed).toBeDefined();
  });

  test("handles missing optional fields", () => {
    const summary = {
      run_id: "01DEF",
      goal: null,
      workflow_slug: null,
      workflow_name: null,
      host_repo_path: null,
      status: "submitted",
      duration_ms: null,
      total_usd_micros: null,
      labels: {},
      start_time: null,
      status_reason: null,
      pending_control: null,
    };
    const item = mapRunSummaryToRunItem(summary);
    expect(item.id).toBe("01DEF");
    expect(item.title).toBe("Untitled run");
    expect(item.workflow).toBe("unknown");
    expect(item.repo).toBe("unknown");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/data/runs.test.ts`
Expected: FAIL (mapRunSummaryToRunItem does not exist yet)

- [ ] **Step 3: Implement `mapRunSummaryToRunItem` and update run-detail loader**

In `apps/fabro-web/app/data/runs.ts`, add:

```typescript
export interface RunSummaryResponse {
  run_id: string;
  goal: string | null;
  workflow_slug: string | null;
  workflow_name: string | null;
  host_repo_path: string | null;
  status: string | null;
  status_reason: string | null;
  pending_control: string | null;
  duration_ms: number | null;
  total_usd_micros: number | null;
  labels: Record<string, string>;
  start_time: string | null;
}

export function mapRunSummaryToRunItem(summary: RunSummaryResponse): RunItem {
  const repoPath = summary.host_repo_path ?? "";
  const repoName = repoPath.split("/").pop() || "unknown";
  return {
    id: summary.run_id,
    repo: repoName,
    title: summary.goal ?? "Untitled run",
    workflow: summary.workflow_slug ?? "unknown",
    elapsed: summary.duration_ms != null
      ? formatElapsedSecs(summary.duration_ms / 1000)
      : undefined,
  };
}
```

In `apps/fabro-web/app/routes/run-detail.tsx`, change the loader:

```typescript
import { RunSummaryResponse, mapRunSummaryToRunItem, columnNames } from "../data/runs";
import type { ColumnStatus } from "../data/runs";
import { apiJson } from "../api";

export async function loader({ request, params }: any) {
  const summary = await apiJson<RunSummaryResponse>(`/runs/${params.id}`, { request });
  const item = mapRunSummaryToRunItem(summary);
  const statusMap: Record<string, ColumnStatus> = {
    running: "working",
    paused: "pending",
    completed: "merge",
  };
  const status = statusMap[summary.status ?? ""] ?? "working";
  return {
    run: {
      ...item,
      status,
      statusLabel: columnNames[status] ?? summary.status ?? "Unknown",
    },
  };
}
```

Remove the `PaginatedRunList` import and the find-by-id logic. Remove the `mapRunListItem` import if no longer needed here.

Note: This works for both real and demo modes because Task 2 ensures the demo `get_run_status` returns `StoreRunSummary`-shaped data with the same fields (`run_id`, `goal`, `workflow_slug`, `host_repo_path`, `duration_ms`, etc.).

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/data/runs.test.ts`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run typecheck and all tests:
Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/fabro-web/app/data/runs.ts apps/fabro-web/app/data/runs.test.ts apps/fabro-web/app/routes/run-detail.tsx
git commit -m "feat(web): use /runs/{id} directly in run-detail loader instead of searching /boards/runs"
```

---

## Task 5: Create `useDemoMode()` hook and `DemoModeProvider` context

**Files:**
- Create: `apps/fabro-web/app/lib/demo-mode.tsx`
- Modify: `apps/fabro-web/app/layouts/app-shell.tsx`

- [ ] **Step 1: Write failing test**

Create `apps/fabro-web/app/lib/demo-mode.test.tsx`:

```typescript
import { describe, expect, test } from "bun:test";
import { renderToString } from "react-dom/server";
import { DemoModeProvider, useDemoMode } from "./demo-mode";

function TestConsumer() {
  const demoMode = useDemoMode();
  return <span data-demo={demoMode}>{demoMode ? "demo" : "prod"}</span>;
}

describe("DemoModeProvider", () => {
  test("provides demo mode value to children", () => {
    const html = renderToString(
      <DemoModeProvider value={true}>
        <TestConsumer />
      </DemoModeProvider>,
    );
    expect(html).toContain("demo");
    expect(html).toContain('data-demo="true"');
  });

  test("defaults to false", () => {
    const html = renderToString(
      <DemoModeProvider value={false}>
        <TestConsumer />
      </DemoModeProvider>,
    );
    expect(html).toContain("prod");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/lib/demo-mode.test.tsx`
Expected: FAIL (module not found)

- [ ] **Step 3: Implement DemoModeProvider and useDemoMode**

Create `apps/fabro-web/app/lib/demo-mode.tsx`:

```tsx
import { createContext, useContext } from "react";

const DemoModeContext = createContext(false);

export function DemoModeProvider({
  value,
  children,
}: {
  value: boolean;
  children: React.ReactNode;
}) {
  return (
    <DemoModeContext.Provider value={value}>
      {children}
    </DemoModeContext.Provider>
  );
}

export function useDemoMode(): boolean {
  return useContext(DemoModeContext);
}
```

In `app-shell.tsx`, wrap the `<Outlet />` with `DemoModeProvider`:

```tsx
import { DemoModeProvider } from "../lib/demo-mode";

// In the component body, wrap the content:
<DemoModeProvider value={demoMode}>
  {/* existing header and main content */}
</DemoModeProvider>
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/lib/demo-mode.test.tsx`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/fabro-web/app/lib/demo-mode.tsx apps/fabro-web/app/lib/demo-mode.test.tsx apps/fabro-web/app/layouts/app-shell.tsx
git commit -m "feat(web): add DemoModeProvider context and useDemoMode hook"
```

---

## Task 6: Conditionally hide nav items and routes based on demo mode

**Files:**
- Modify: `apps/fabro-web/app/layouts/app-shell.tsx`
- Modify: `apps/fabro-web/app/routes/run-detail.tsx`

- [ ] **Step 1: Write failing test**

This is a visual behavior change. We will verify with the Playwright browser test in Task 9. For now, write a unit test verifying the navigation filtering logic.

Create `apps/fabro-web/app/layouts/app-shell.test.tsx`:

```typescript
import { describe, expect, test } from "bun:test";

// Test the navigation filtering logic extracted as a pure function
import { getVisibleNavigation } from "./app-shell";

describe("getVisibleNavigation", () => {
  test("shows all nav items in demo mode", () => {
    const items = getVisibleNavigation(true);
    const names = items.map((i) => i.name);
    expect(names).toContain("Workflows");
    expect(names).toContain("Runs");
    expect(names).toContain("Insights");
  });

  test("hides Workflows and Insights in production mode", () => {
    const items = getVisibleNavigation(false);
    const names = items.map((i) => i.name);
    expect(names).not.toContain("Workflows");
    expect(names).not.toContain("Insights");
    expect(names).toContain("Runs");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/layouts/app-shell.test.tsx`
Expected: FAIL (getVisibleNavigation not exported)

- [ ] **Step 3: Extract navigation filtering and conditionally hide items**

In `app-shell.tsx`:

1. Export the navigation array and a filtering function:

```typescript
const allNavigation = [
  { name: "Workflows", href: "/workflows", icon: RectangleStackIcon, demoOnly: true },
  { name: "Runs", href: "/runs", icon: PlayIcon, demoOnly: false },
  { name: "Insights", href: "/insights", icon: ChartBarIcon, demoOnly: true },
];

export function getVisibleNavigation(demoMode: boolean) {
  return allNavigation.filter((item) => !item.demoOnly || demoMode);
}
```

2. In the component, use `getVisibleNavigation(demoMode)` instead of the static `navigation` array.

In `run-detail.tsx`, conditionally filter the tabs array based on demo mode. Remove "Stages" and "Settings" tabs when not in demo mode. Remove "Files Changed" tab always (the `/runs/{id}/files` endpoint does not exist in either mode). Keep "Overview", "Graph", and "Billing":

```typescript
import { useDemoMode } from "../lib/demo-mode";

// Define all tabs
const allTabs = [
  { name: "Overview", path: "", count: null, demoOnly: false, broken: false },
  { name: "Stages", path: "/stages/detect-drift", count: null, demoOnly: true, broken: false },
  { name: "Files Changed", path: "/files", count: null, demoOnly: false, broken: true },
  { name: "Graph", path: "/graph", count: null, demoOnly: false, broken: false },
  { name: "Billing", path: "/billing", count: null, demoOnly: false, broken: false },
];

// In component:
const demoMode = useDemoMode();
const visibleTabs = allTabs.filter((t) => !t.broken && (!t.demoOnly || demoMode));
```

Note: The original tabs array has "Overview", "Stages", "Files Changed", "Billing" -- it does not include "Graph". Add "Graph" to the tabs since the graph tab route exists and works in real mode. The "Settings" tab is not listed in the current tabs array (the route exists but has no tab link), so it is already effectively hidden.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/layouts/app-shell.test.tsx`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/fabro-web/app/layouts/app-shell.tsx apps/fabro-web/app/layouts/app-shell.test.tsx apps/fabro-web/app/routes/run-detail.tsx
git commit -m "feat(web): hide Workflows, Insights nav and demo-only run tabs in production mode"
```

---

## Task 7: Add `apiJsonOrNull` helper and make run-overview/run-graph loaders resilient

**Files:**
- Modify: `apps/fabro-web/app/api.ts`
- Modify: `apps/fabro-web/app/routes/run-overview.tsx`
- Modify: `apps/fabro-web/app/routes/run-graph.tsx`

- [ ] **Step 1: Write failing test**

Create `apps/fabro-web/app/api.test.ts`:

```typescript
import { describe, expect, test } from "bun:test";

// We test the logic of apiJsonOrNull which returns null on 501
// Since we can't mock fetch easily, test the extraction function
import { isNotImplemented } from "./api";

describe("isNotImplemented", () => {
  test("returns true for 501 status", () => {
    expect(isNotImplemented(501)).toBe(true);
  });

  test("returns false for 200 status", () => {
    expect(isNotImplemented(200)).toBe(false);
  });

  test("returns false for 404 status", () => {
    expect(isNotImplemented(404)).toBe(false);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/api.test.ts`
Expected: FAIL (isNotImplemented not exported)

- [ ] **Step 3: Implement `apiJsonOrNull` and `isNotImplemented`, update loaders**

In `apps/fabro-web/app/api.ts`, add:

```typescript
export function isNotImplemented(status: number): boolean {
  return status === 501;
}

export async function apiJsonOrNull<T>(path: string, options?: ApiOptions): Promise<T | null> {
  const response = await apiFetch(path, options);
  if (isNotImplemented(response.status)) {
    return null;
  }
  if (!response.ok) {
    throw new Response(null, { status: response.status, statusText: response.statusText });
  }
  return response.json() as Promise<T>;
}
```

In `run-overview.tsx`, simplify the loader to only fetch stages (gracefully) and set `graphDot` to null:

```typescript
import { apiJsonOrNull } from "../api";

export async function loader({ request, params }: any) {
  const stagesResult = await apiJsonOrNull<PaginatedRunStageList>(
    `/runs/${params.id}/stages`,
    { request },
  );
  const stages: Stage[] = (stagesResult?.data ?? []).map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  return { stages, graphDot: null };
}
```

Remove the imports for `PaginatedRunList`, `WorkflowDetailResponse`, and `apiJson` (if no longer needed). Keep `apiJsonOrNull`.

In `run-graph.tsx`, use `apiJsonOrNull` for stages:

```typescript
import { apiJsonOrNull } from "../api";

export async function loader({ request, params }: any) {
  const [stagesResult, graphRes] = await Promise.all([
    apiJsonOrNull<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiFetch(`/runs/${params.id}/graph`, { request }),
  ]);
  const stages: Stage[] = (stagesResult?.data ?? []).map((s) => ({
    id: s.id,
    name: s.name,
    dotId: s.dot_id ?? s.id,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  const graphSvg = graphRes.ok ? await graphRes.text() : null;
  return { stages, graphSvg };
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun test app/api.test.ts`
Expected: PASS

- [ ] **Step 5: Refactor and verify**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/fabro-web/app/api.ts apps/fabro-web/app/api.test.ts apps/fabro-web/app/routes/run-overview.tsx apps/fabro-web/app/routes/run-graph.tsx
git commit -m "feat(web): add apiJsonOrNull for graceful 501 handling in run-overview and run-graph"
```

---

## Task 8: Set up Playwright and write browser smoke tests

**Files:**
- Create: `apps/fabro-web/tests/playwright.config.ts`
- Create: `apps/fabro-web/tests/browser/smoke.test.ts`
- Modify: `apps/fabro-web/package.json` (add test:browser script)

- [ ] **Step 1: Install Playwright and configure**

```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web
bun add -d @playwright/test
```

Create `apps/fabro-web/tests/playwright.config.ts`:

```typescript
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/browser",
  timeout: 30000,
  use: {
    baseURL: "http://localhost:8080",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "cd ../.. && FABRO_TEST_IN_MEMORY_STORE=1 cargo run -p fabro-cli -- server foreground --bind 127.0.0.1:8080",
    port: 8080,
    reuseExistingServer: true,
    timeout: 120000,
  },
});
```

Note: The exact server start command may need adjustment. The fabro server serves the built SPA via static file handler when `FABRO_STATIC_DIR` points to the built web app. The test should build the web app first, then start the server with auth disabled. Check the `ServeArgs` in `serve.rs` and the CLI subcommand in `commands/server/` for the correct invocation. If `server foreground` does not work, try `fabro serve` or adjust. The key settings are:
- `FABRO_TEST_IN_MEMORY_STORE=1` for an ephemeral store
- Auth mode defaults to `AuthMode::Disabled` when no auth configuration is present
- The static file handler serves from the configured static directory

- [ ] **Step 2: Write browser smoke tests**

Create `apps/fabro-web/tests/browser/smoke.test.ts`:

```typescript
import { test, expect } from "@playwright/test";

test.describe("Production mode (no demo header)", () => {
  test("runs board loads without errors", async ({ page }) => {
    await page.goto("/runs");
    // Should not show error page
    await expect(page.locator("body")).not.toContainText("Unauthorized");
    // Take screenshot for visual verification
    await page.screenshot({ path: "test-results/runs-prod.png" });
  });

  test("settings page loads", async ({ page }) => {
    await page.goto("/settings");
    await expect(page.locator("body")).not.toContainText("Not implemented");
    await page.screenshot({ path: "test-results/settings-prod.png" });
  });

  test("navigation does not show Workflows in production mode", async ({ page }) => {
    await page.goto("/runs");
    const nav = page.locator("nav");
    await expect(nav).not.toContainText("Workflows");
    await expect(nav).not.toContainText("Insights");
    await expect(nav).toContainText("Runs");
  });
});

test.describe("Demo mode (with demo cookie)", () => {
  test.beforeEach(async ({ context }) => {
    await context.addCookies([{
      name: "fabro-demo",
      value: "1",
      domain: "localhost",
      path: "/",
    }]);
  });

  test("runs board loads with demo data", async ({ page }) => {
    await page.goto("/runs");
    // Demo mode should show run cards
    await expect(page.locator("body")).not.toContainText("error");
    await page.screenshot({ path: "test-results/runs-demo.png" });
  });

  test("navigation shows all items in demo mode", async ({ page }) => {
    await page.goto("/runs");
    const nav = page.locator("nav");
    await expect(nav).toContainText("Workflows");
    await expect(nav).toContainText("Runs");
    await expect(nav).toContainText("Insights");
  });

  test("workflows page loads in demo mode", async ({ page }) => {
    await page.goto("/workflows");
    await expect(page.locator("body")).not.toContainText("error");
    await page.screenshot({ path: "test-results/workflows-demo.png" });
  });

  test("insights page loads in demo mode", async ({ page }) => {
    await page.goto("/insights");
    await expect(page.locator("body")).not.toContainText("error");
    await page.screenshot({ path: "test-results/insights-demo.png" });
  });
});

test.describe("Demo mode toggle", () => {
  test("toggling demo mode changes navigation", async ({ page }) => {
    // Start in prod mode
    await page.goto("/runs");
    const nav = page.locator("nav");
    await expect(nav).not.toContainText("Workflows");

    // Toggle demo mode on via the beaker button
    const demoToggle = page.locator('button[title*="demo"], button[title*="Demo"]');
    await demoToggle.click();

    // Wait for revalidation
    await page.waitForTimeout(1000);
    await expect(nav).toContainText("Workflows");
    await page.screenshot({ path: "test-results/after-toggle-demo.png" });
  });
});
```

- [ ] **Step 3: Add test script to package.json**

In `apps/fabro-web/package.json`, add:

```json
"test:browser": "bunx playwright test --config tests/playwright.config.ts"
```

- [ ] **Step 4: Build and run browser tests**

First build the web app so the server can serve it:
```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run build
```

Then run the browser tests (this requires the fabro server to be running or the webServer config to start it):
```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run test:browser
```

Expected: Tests may fail on first run due to the server startup command or auth configuration. Iterate on the Playwright config's `webServer.command` until the server starts correctly with auth disabled and serves the built SPA. The server's `AuthMode::Disabled` skips authentication, and `getAuthMe()` should still return a response (it returns a disabled-mode user). If `getAuthMe()` returns 401 in disabled mode, that indicates the server auth isn't properly disabled -- check the environment variables and CLI flags.

- [ ] **Step 5: Refactor and verify**

Run all tests including browser:
```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test && bun run test:browser
```
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add apps/fabro-web/tests/ apps/fabro-web/package.json
git commit -m "test(web): add Playwright browser smoke tests for production and demo mode"
```

---

## Task 9: Final integration verification

- [ ] **Step 1: Run all Rust tests**

```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && ulimit -n 4096 && cargo nextest run -p fabro-server
```
Expected: all PASS

- [ ] **Step 2: Run all TypeScript tests**

```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test
```
Expected: all PASS

- [ ] **Step 3: Build production web app**

```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run build
```
Expected: Build succeeds with no errors

- [ ] **Step 4: Run browser tests**

```bash
cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run test:browser
```
Expected: all PASS

- [ ] **Step 5: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final integration cleanup for production web UI"
```

---

## Summary of changes by endpoint

| Endpoint | Real mode | Demo mode | UI behavior |
|---|---|---|---|
| `/boards/runs` | Enriched to return `RunListItem` with board columns | New handler delegates to `list_runs` | Runs board works in both modes |
| `/runs/{id}` | Returns `RunSummary` (already works) | Fixed to return `StoreRunSummary` shape (was returning `RunStatusResponse`) | Run detail uses this directly |
| `/runs/{id}/graph` | Returns SVG (already works) | Returns SVG | Graph tab works in both modes |
| `/runs/{id}/billing` | Returns billing (already works) | Returns billing | Billing tab works in both modes |
| `/runs/{id}/stages` | Returns 501 | Returns demo stages | Graceful null in real mode; full data in demo |
| `/runs/{id}/stages/{stageId}/turns` | Returns 501 | Returns demo turns | Tab hidden in real mode |
| `/runs/{id}/settings` | Returns 501 | Returns demo settings | Tab hidden in real mode |
| `/runs/{id}/files` | Does not exist | Does not exist | Tab always hidden (endpoint missing in both modes) |
| `/workflows` | Returns 501 | Returns demo workflows | Nav hidden in real mode |
| `/workflows/{name}` | Returns 501 | Returns demo detail | Nav hidden in real mode |
| `/workflows/{name}/runs` | Returns 501 | Returns demo runs | Nav hidden in real mode |
| `/insights/*` | Returns 501 | Returns demo data | Nav hidden in real mode |
| `/settings` | Returns settings (works) | Returns demo settings | Works in both modes |
