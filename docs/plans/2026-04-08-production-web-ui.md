# Production Web UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use trycycle-executing to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the fabro web UI to the real fabro server via HTTP, retain a demo mode toggle, and remove UI for server features that are not implemented in real mode.

**Architecture:** The web UI already calls the fabro server's `/api/v1/` endpoints via `apiFetch`/`apiJson` helpers. Demo mode is toggled per-request via a `fabro-demo` cookie that the server middleware converts to an `X-Fabro-Demo: 1` header, dispatching to separate demo vs real route sets. The core work is: (1) fix the real `/boards/runs` handler to return the `RunListItem` shape the UI expects, (2) make the UI conditionally hide navigation and routes for features not implemented in real mode (workflows, insights, run files), and (3) add a demo `/boards/runs` handler so demo mode also works through this endpoint.

**Tech Stack:** Rust (Axum server), TypeScript (React 19 + React Router + Vite), Playwright (browser tests)

---

## Key architectural decisions

### Decision 1: Fix real `/boards/runs` to return `RunListItem` shape

The OpenAPI spec declares `/boards/runs` returns `PaginatedRunList` containing `RunListItem` objects. However, the real `list_board_runs` handler currently returns `RunStatusResponse` objects (id, status, error, queue_position, created_at). The UI's runs board, run-detail, and run-overview loaders all consume `/boards/runs` and expect `RunListItem` fields (repository, title, workflow, status as `BoardColumn`, pull_request, timings, sandbox, question).

**Decision:** Enrich the real `list_board_runs` handler to return `RunListItem`-shaped data by pulling `goal` (as title), `workflow_slug`/`workflow_name`, `host_repo_path` (as repository name), `duration_ms` (as timing), and `total_usd_micros` from `RunSummary`. Map `RunStatus` lifecycle values to `BoardColumn` values: `running` -> `working`, `paused` -> `pending`, `completed` -> `merge`, everything else (`submitted`, `queued`, `starting`, `failed`, `cancelled`) -> excluded from the board (they are not actionable board items).

**Justification:** This aligns the real handler with the OpenAPI spec and avoids bifurcating the UI's data layer into two incompatible response shapes. The store already has the needed fields. Fields not available from the store (pull_request, sandbox, checks, question) are left `null`/absent -- the UI already handles their optionality with `?.` chains.

### Decision 2: Add `/boards/runs` to demo routes

The demo routes currently have `/runs` but NOT `/boards/runs`. The UI exclusively calls `/boards/runs` for the runs board. Since the server dispatches to demo vs real routes based on the `X-Fabro-Demo` header, and both route sets need `/boards/runs`, add it to demo routes.

**Decision:** Add a `demo::list_board_runs` handler that returns the same `RunListItem` data the existing `demo::list_runs` returns, but under the `/boards/runs` path.

### Decision 3: Conditionally hide unimplemented features based on demo mode

The real server returns `not_implemented` (501) for: `/workflows`, `/workflows/{name}`, `/workflows/{name}/runs`, `/insights/*`, `/runs/{id}/stages`, `/runs/{id}/stages/{stageId}/turns`, `/runs/{id}/settings`, and `/runs/{id}/files` (doesn't exist at all).

**Decision:** The `auth/me` response already includes `demoMode: boolean`. Use this flag in the UI to:
- Hide the "Workflows" and "Insights" nav items when not in demo mode
- Remove the "Stages", "Files Changed", and "Settings" tabs from the run detail view when not in demo mode (keep Overview, Graph, Billing which all use real endpoints)
- Redirect away from workflow/insight routes when not in demo mode

This avoids showing users broken pages. The routes remain in the router for demo mode.

**Justification:** Per user instruction: "for functionality that has been removed from fabro server, remove the corresponding UI from fabro web for now." Using `demoMode` from the existing auth response is the simplest mechanism -- no new API call needed.

### Decision 4: Run overview graceful degradation

The run-overview loader fetches both `/runs/{id}/stages` (501 in real mode) and `/boards/runs`. It also tries to fetch `/workflows/{name}` for the graph dot source.

**Decision:** Make the run-overview loader resilient: catch 501 errors from `/runs/{id}/stages` and return an empty stages list. Catch errors from `/workflows/{name}` (already caught) and leave `graphDot` null. The overview page already renders conditionally when these are missing.

### Decision 5: Run detail loader -- use `/runs/{id}` instead of searching `/boards/runs`

The run-detail loader currently fetches ALL board runs via `/boards/runs` and finds the run by ID. This is wasteful and won't scale.

**Decision:** Change run-detail loader to fetch `/runs/{id}` directly. The real handler returns `RunSummary` (which contains `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`). Map this to the same shape the component expects. For demo mode, this endpoint (`demo::get_run_status`) already returns compatible data.

### Decision 6: Propagate `demoMode` via React context

Currently `demoMode` is only available in the app-shell loader data. Child routes need it to conditionally render features.

**Decision:** The app-shell already passes `demoMode` from `getAuthMe()`. Add it to React Router's `useRouteLoaderData` pattern -- child routes can access the app shell's loader data through `useMatches()` or a dedicated hook. Implement a small `useDemoMode()` hook that reads from the nearest parent match.

### Decision 7: Workflow-definition uses hardcoded static data

`workflow-definition.tsx` imports `workflowData` from `workflow-detail.tsx` and reads from the static record by name, ignoring the loader data. This is a demo-only artifact.

**Decision:** Since workflows are demo-only for now, this is acceptable. No change needed -- the route is only accessible in demo mode.

---

## File structure

### Files to modify

- `lib/crates/fabro-server/src/server.rs` -- Enrich real `list_board_runs` to return `RunListItem` shape; no changes to routes
- `lib/crates/fabro-server/src/demo/mod.rs` -- Add `list_board_runs` handler reusing existing run data
- `apps/fabro-web/app/layouts/app-shell.tsx` -- Conditionally hide nav items based on `demoMode`; export demo mode via context
- `apps/fabro-web/app/lib/demo-mode.tsx` -- New file: `DemoModeProvider` context and `useDemoMode()` hook
- `apps/fabro-web/app/routes/runs.tsx` -- Use `/boards/runs` (already does); no structural changes needed
- `apps/fabro-web/app/routes/run-detail.tsx` -- Change loader to use `/runs/{id}` instead of searching `/boards/runs`; conditionally hide tabs
- `apps/fabro-web/app/routes/run-overview.tsx` -- Make loader resilient to 501 from stages endpoint; remove `/boards/runs` dependency
- `apps/fabro-web/app/routes/run-stages.tsx` -- No loader changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/run-graph.tsx` -- Make loader resilient to 501 from stages endpoint; use `/runs/{id}/graph` (real, works)
- `apps/fabro-web/app/routes/run-settings.tsx` -- No loader changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/run-files.tsx` -- No loader changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/run-billing.tsx` -- No changes; uses `/runs/{id}/billing` which is implemented in real mode
- `apps/fabro-web/app/routes/workflows.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/workflow-detail.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/insights.tsx` -- No changes; route hidden in non-demo mode
- `apps/fabro-web/app/routes/settings.tsx` -- No changes; uses `/settings` which is implemented in real mode
- `apps/fabro-web/app/data/runs.ts` -- Add `mapRunStatusToRunItem()` for mapping `/runs/{id}` response
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

## Task 2: Enrich real `/boards/runs` to return `RunListItem` shape

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

Check that existing tests still pass. The existing tests that call `/boards/runs` and assert on `status_reason`/`pending_control` will need to be updated -- they assert on the old `RunStatusResponse` shape. Update those tests to assert on the new `RunListItem` shape, or adjust assertions to check fields that both shapes share.

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui && ulimit -n 4096 && cargo nextest run -p fabro-server`
Expected: all PASS

- [ ] **Step 6: Commit**

```bash
git add lib/crates/fabro-server/src/server.rs
git commit -m "feat(server): enrich /boards/runs to return RunListItem shape with board columns"
```

---

## Task 3: Fix run-detail loader to use `/runs/{id}` directly

**Files:**
- Modify: `apps/fabro-web/app/routes/run-detail.tsx:19-30`
- Modify: `apps/fabro-web/app/data/runs.ts`

- [ ] **Step 1: Write failing test**

Add a TypeScript test in `apps/fabro-web/app/data/runs.test.ts` that tests a new `mapRunSummaryToRunItem()` function which maps the `/runs/{id}` response shape (a `RunSummary` with `run_id`, `goal`, `workflow_slug`, `workflow_name`, `host_repo_path`, `status`, `duration_ms`) to the `RunItem` shape.

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

Also import `RunSummaryResponse` and `mapRunSummaryToRunItem` from `../data/runs`, and remove the `PaginatedRunList` import and the find-by-id logic.

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

## Task 4: Create `useDemoMode()` hook and `DemoModeProvider` context

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

## Task 5: Conditionally hide nav items and routes based on demo mode

**Files:**
- Modify: `apps/fabro-web/app/layouts/app-shell.tsx`
- Modify: `apps/fabro-web/app/routes/run-detail.tsx`

- [ ] **Step 1: Write failing test**

This is a visual behavior change. We will verify with the Playwright browser test in Task 8. For now, write a unit test verifying the navigation filtering logic.

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

In `run-detail.tsx`, conditionally filter the tabs array based on demo mode. Remove "Stages", "Files Changed" tabs when not in demo mode. Keep "Overview", "Graph" (which will gracefully degrade), and "Billing":

```typescript
import { useDemoMode } from "../lib/demo-mode";

// In component:
const demoMode = useDemoMode();
const visibleTabs = demoMode
  ? tabs
  : tabs.filter((t) => !["Stages", "Files Changed"].includes(t.name));
```

Note: The "Settings" tab for run-settings also calls `/runs/{id}/settings` (not_implemented). Remove it from non-demo tabs too.

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

## Task 6: Add `apiJsonOrNull` helper and make run-overview/run-graph loaders resilient

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

In `run-overview.tsx`, change the loader to use `apiJsonOrNull` for stages:

```typescript
import { apiJson, apiJsonOrNull } from "../api";

export async function loader({ request, params }: any) {
  const [stagesResult, response] = await Promise.all([
    apiJsonOrNull<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiJson<PaginatedRunList>("/boards/runs", { request }),
  ]);
  const stages: Stage[] = (stagesResult?.data ?? []).map((s) => ({
    id: s.id,
    name: s.name,
    status: s.status as StageStatus,
    duration: s.duration_secs != null ? formatDurationSecs(s.duration_secs) : "--",
  }));
  // ... rest unchanged
}
```

In `run-graph.tsx`, similarly use `apiJsonOrNull` for stages:

```typescript
export async function loader({ request, params }: any) {
  const [stagesResult, graphRes] = await Promise.all([
    apiJsonOrNull<PaginatedRunStageList>(`/runs/${params.id}/stages`, { request }),
    apiFetch(`/runs/${params.id}/graph`, { request }),
  ]);
  const stages: Stage[] = (stagesResult?.data ?? []).map((s) => ({
    // ... same as before
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

## Task 7: Fix run-overview loader to not depend on `/boards/runs` for the current run

**Files:**
- Modify: `apps/fabro-web/app/routes/run-overview.tsx:24-46`

The run-overview loader currently fetches `/boards/runs` to find the current run and get its `workflow` slug for the graph. Since run-detail now fetches `/runs/{id}` directly, the overview can use `useRouteLoaderData` or receive the run from the parent layout (run-detail), OR it can fetch `/runs/{id}` itself.

- [ ] **Step 1: Identify the issue**

The run-overview loader fetches `/boards/runs` just to find the current run's workflow slug so it can fetch `/workflows/{name}` for the graph dot source. But `/workflows/{name}` is `not_implemented` in real mode anyway. And the run graph is available at `/runs/{id}/graph` (which IS implemented in real mode).

So the overview loader should:
1. Fetch stages via `apiJsonOrNull` (done in Task 6)
2. NOT fetch `/boards/runs` at all
3. NOT fetch `/workflows/{name}` -- the graph dot source is only useful for Graphviz rendering, and the run-graph tab already handles this

- [ ] **Step 2: Simplify run-overview loader**

Remove the `/boards/runs` fetch and the `/workflows/{name}` fetch from the run-overview loader. Set `graphDot` to `null` -- the overview page's mini graph section will simply not render when `graphDot` is null (it already has conditional rendering).

```typescript
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

- [ ] **Step 3: Run tests and typecheck**

Run: `cd /Users/bhelmkamp/p/fabro-sh/fabro-3/.worktrees/production-web-ui/apps/fabro-web && bun run typecheck && bun test`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add apps/fabro-web/app/routes/run-overview.tsx
git commit -m "refactor(web): simplify run-overview loader to remove /boards/runs and /workflows dependencies"
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
    command: "cd ../.. && cargo run -p fabro-server -- serve --port 8080",
    port: 8080,
    reuseExistingServer: true,
    timeout: 120000,
  },
});
```

Note: The exact server start command may need adjustment. The fabro server serves the built SPA via static file handler. The test should build the web app first, then start the server with auth disabled.

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

    // Toggle demo mode on
    const demoToggle = page.locator('button[title*="demo"]');
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

Expected: Tests may fail on first run due to auth requirements. Adjust the Playwright config and tests to handle `AuthMode::Disabled`. The server must be started with auth disabled for the tests to work. Iterate until all smoke tests pass.

Note: The exact server startup command and auth handling will need adjustment. The key insight is that in `AuthMode::Disabled`, the server skips authentication, and `getAuthMe()` should still return a response (it returns a disabled-mode user). Verify this works, and if `getAuthMe()` returns 401 in disabled mode, handle the redirect in tests.

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
| `/runs/{id}` | Returns `RunSummary` (already works) | Returns demo run status | Run detail uses this directly |
| `/runs/{id}/graph` | Returns SVG (already works) | Returns SVG | Graph tab works in both modes |
| `/runs/{id}/billing` | Returns billing (already works) | Returns billing | Billing tab works in both modes |
| `/runs/{id}/stages` | Returns 501 | Returns demo stages | Graceful null in real mode; full data in demo |
| `/runs/{id}/stages/{stageId}/turns` | Returns 501 | Returns demo turns | Tab hidden in real mode |
| `/runs/{id}/settings` | Returns 501 | Returns demo settings | Tab hidden in real mode |
| `/runs/{id}/files` | Does not exist | Does not exist | Tab hidden always (remove) |
| `/workflows` | Returns 501 | Returns demo workflows | Nav hidden in real mode |
| `/workflows/{name}` | Returns 501 | Returns demo detail | Nav hidden in real mode |
| `/workflows/{name}/runs` | Returns 501 | Returns demo runs | Nav hidden in real mode |
| `/insights/*` | Returns 501 | Returns demo data | Nav hidden in real mode |
| `/settings` | Returns settings (works) | Returns demo settings | Works in both modes |
| `/models` | Returns models (works) | Returns models | Works in both modes |
| `/secrets` | Returns secrets (works) | Returns demo secrets | Works in both modes |
| `/demo/toggle` | Sets cookie | Sets cookie | Toggle works in both modes |
| `/auth/me` | Returns user + demoMode | Returns user + demoMode | Used for auth and demo mode flag |
