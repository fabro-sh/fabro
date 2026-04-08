# Production Web UI Test Plan

## Harness requirements

### 1. Rust HTTP integration harness (existing)

- **What it does:** Sends HTTP requests to the fabro server via `tower::ServiceExt::oneshot()` against an in-memory `AppState`. No network, no boot overhead.
- **What it exposes:** Full request/response cycle including headers, status codes, JSON bodies. Direct mutation of `AppState` (e.g., setting run status) for precondition setup.
- **Estimated complexity:** Already exists (`lib/crates/fabro-server/tests/it/helpers.rs`). Provides `test_app_state()`, `build_router()`, `body_json()`, `create_and_start_run()`, `api()`, etc.
- **Which tests depend on it:** Tests 1-6

### 2. Playwright browser harness (new -- must be built)

- **What it does:** Launches a real browser against a running fabro server that serves the built SPA. Exercises the full stack: React app fetching real HTTP endpoints, rendering real DOM, cookies for demo mode.
- **What it exposes:** Page navigation, DOM element inspection, cookie management, screenshot capture, click/interaction simulation.
- **Estimated complexity:** Moderate. Requires:
  1. Install `@playwright/test` as a dev dependency in `apps/fabro-web`
  2. Create `apps/fabro-web/tests/playwright.config.ts` with `webServer` config that builds the SPA and starts the fabro server with `AuthMode::Disabled` and in-memory store
  3. The server already serves the built SPA when `FABRO_STATIC_DIR` is configured. Auth disabled mode returns a synthetic user from `/auth/me`.
- **Which tests depend on it:** Tests 7-14

### 3. TypeScript unit test harness (existing)

- **What it does:** Runs bun tests for pure TypeScript functions and React component logic.
- **What it exposes:** Direct function calls, React `renderToString` for context providers.
- **Estimated complexity:** Already exists (`bun test`).
- **Which tests depend on it:** Tests 15-18

---

## Test plan

### Test 1: Demo `/boards/runs` returns RunListItem-shaped data

- **Name:** Requesting the runs board in demo mode returns run list items with repository, title, workflow, and board column status
- **Type:** integration
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized with `AuthMode::Disabled`. Demo header `X-Fabro-Demo: 1` set on request.
- **Actions:** `GET /api/v1/boards/runs` with `X-Fabro-Demo: 1` header
- **Expected outcome:** HTTP 200. Response body has `data` array. First item has string `id`, object `repository` (with `name`), string `title`, object `workflow` (with `slug`), string `status` that is one of `"working"`, `"pending"`, `"review"`, `"merge"`, and string `created_at`. Source of truth: OpenAPI spec `PaginatedRunList` schema and plan Decision 2.
- **Interactions:** Demo data module (`demo/mod.rs` `runs::list_items()`)

### Test 2: Demo `GET /runs/{id}` returns StoreRunSummary shape (not RunStatusResponse)

- **Name:** Requesting a single run in demo mode returns the StoreRunSummary shape with run_id, goal, and workflow fields instead of the old RunStatusResponse shape
- **Type:** integration
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized with `AuthMode::Disabled`. Demo header set. Run ID "run-1" exists in demo data.
- **Actions:** `GET /api/v1/runs/run-1` with `X-Fabro-Demo: 1` header
- **Expected outcome:** HTTP 200. Body has `run_id` (string), `goal` (string), `workflow_slug` (string), `workflow_name` (string), `host_repo_path` (string), `status` (string), `duration_ms` (number or null). Body does NOT have `queue_position` field. Source of truth: OpenAPI spec `StoreRunSummary` schema and plan Decision 3.
- **Interactions:** Demo data module

### Test 3: Demo `GET /runs/{id}` returns 404 for unknown run

- **Name:** Requesting a nonexistent run in demo mode returns 404
- **Type:** boundary
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized with `AuthMode::Disabled`. Demo header set.
- **Actions:** `GET /api/v1/runs/nonexistent-run-id` with `X-Fabro-Demo: 1` header
- **Expected outcome:** HTTP 404. Source of truth: OpenAPI spec 404 error response.
- **Interactions:** Demo data module

### Test 4: Real `/boards/runs` returns RunListItem shape with board columns

- **Name:** Requesting the runs board in production mode returns enriched run list items with board column statuses instead of lifecycle statuses
- **Type:** integration
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized with `AuthMode::Disabled`, no demo header. A run created and started, then set to `RunStatus::Running` in state.
- **Actions:** `GET /api/v1/boards/runs` (no demo header)
- **Expected outcome:** HTTP 200. Response has `data` array. The created run appears with string `id`, string `title`, object `repository` (with `name`), object `workflow` (with `slug`), string `status` equal to `"working"` (the board column mapping of `Running`), and string `created_at`. Source of truth: OpenAPI spec `PaginatedRunList` schema and plan Decision 1 (Running -> "working").
- **Interactions:** `state.runs` mutex, `state.store.list_runs()`

### Test 5: Real `/boards/runs` excludes non-board statuses

- **Name:** Runs with statuses that don't map to board columns (Submitted, Queued, Starting, Failed, Cancelled) are excluded from the board response
- **Type:** boundary
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized. A run created and started, then set to `RunStatus::Failed`.
- **Actions:** `GET /api/v1/boards/runs` (no demo header)
- **Expected outcome:** HTTP 200. The failed run does NOT appear in the `data` array. Source of truth: Plan Decision 1 status mapping -- Failed is excluded.
- **Interactions:** `state.runs` mutex

### Test 6: Real `/boards/runs` maps Paused to "pending" and Completed to "merge"

- **Name:** Board column mapping correctly translates Paused to pending and Completed to merge
- **Type:** integration
- **Disposition:** new
- **Harness:** Rust HTTP integration harness
- **Preconditions:** Server initialized. Two runs created: one set to `RunStatus::Paused`, one set to `RunStatus::Completed`.
- **Actions:** `GET /api/v1/boards/runs` (no demo header)
- **Expected outcome:** HTTP 200. Paused run has `status: "pending"`. Completed run has `status: "merge"`. Source of truth: Plan Decision 1 status mapping.
- **Interactions:** `state.runs` mutex, `state.store.list_runs()`

### Test 7: Runs board loads without errors in production mode (browser)

- **Name:** Navigating to the runs board in production mode renders a page without error messages
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Fabro server running with auth disabled and in-memory store. SPA built and served. No demo cookie set.
- **Actions:** Navigate to `/runs`.
- **Expected outcome:** Page loads. Body does not contain "Unauthorized" or "500" or "error" (case-insensitive check excluding expected UI text). Screenshot captured as artifact at `test-results/runs-prod.png`. Source of truth: User request ("make it production grade" -- the runs board is the primary view and must render).
- **Interactions:** React Router loader -> `apiFetch("/boards/runs")` -> server real `list_board_runs` handler

### Test 8: Navigation hides Workflows and Insights in production mode (browser)

- **Name:** The sidebar/header navigation does not show Workflows or Insights links when not in demo mode
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running, no demo cookie.
- **Actions:** Navigate to `/runs`. Inspect the `nav` element.
- **Expected outcome:** `nav` element does NOT contain text "Workflows". `nav` element does NOT contain text "Insights". `nav` element DOES contain text "Runs". Source of truth: Plan Decision 4 and user request ("remove the corresponding UI from fabro web for now").
- **Interactions:** App shell loader -> `getAuthMe()` -> `demoMode: false` -> `getVisibleNavigation(false)` filters out demo-only items

### Test 9: Settings page loads in production mode (browser)

- **Name:** The settings page loads successfully in production mode since /settings is implemented in the real server
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running, no demo cookie.
- **Actions:** Navigate to `/settings`.
- **Expected outcome:** Page loads without "Not implemented" or "500" error text. Screenshot captured at `test-results/settings-prod.png`. Source of truth: Real server route table has `get(get_server_settings)` for `/settings`.
- **Interactions:** Settings route loader -> `apiJson("/settings")` -> real `get_server_settings` handler

### Test 10: Demo mode runs board shows demo data (browser)

- **Name:** Navigating to the runs board with the demo cookie set renders demo run data
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running. `fabro-demo=1` cookie set for localhost.
- **Actions:** Navigate to `/runs`.
- **Expected outcome:** Page loads without error. Body content is not empty/blank (confirms demo data rendered). Screenshot captured at `test-results/runs-demo.png`. Source of truth: Plan Decision 2 (demo `/boards/runs` returns demo run list items).
- **Interactions:** Demo cookie -> server `X-Fabro-Demo` middleware -> demo `list_board_runs` -> demo run data

### Test 11: Navigation shows all items in demo mode (browser)

- **Name:** The navigation shows Workflows, Runs, and Insights links when in demo mode
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running. Demo cookie set.
- **Actions:** Navigate to `/runs`. Inspect `nav` element.
- **Expected outcome:** `nav` contains "Workflows", "Runs", and "Insights". Source of truth: Plan Decision 4 -- all nav items visible in demo mode.
- **Interactions:** App shell loader -> `getAuthMe()` -> `demoMode: true` -> `getVisibleNavigation(true)` includes all items

### Test 12: Workflows page loads in demo mode (browser)

- **Name:** The workflows page renders successfully when in demo mode
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running. Demo cookie set.
- **Actions:** Navigate to `/workflows`.
- **Expected outcome:** Page loads without "error" or "500" text. Screenshot captured at `test-results/workflows-demo.png`. Source of truth: Demo routes include workflow handlers.
- **Interactions:** Workflows route loader -> demo workflow handlers

### Test 13: Toggling demo mode changes navigation (browser)

- **Name:** Clicking the demo toggle button switches between production and demo navigation
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running. No demo cookie initially.
- **Actions:**
  1. Navigate to `/runs`
  2. Verify nav does NOT contain "Workflows"
  3. Click the beaker button (demo toggle -- `button[title*="demo"], button[title*="Demo"]`)
  4. Wait for page to revalidate
  5. Verify nav now DOES contain "Workflows"
- **Expected outcome:** Before toggle: no "Workflows" in nav. After toggle: "Workflows" appears in nav. Screenshot captured at `test-results/after-toggle-demo.png`. Source of truth: Plan Decision 4 -- `toggleDemoMode()` posts to `/api/v1/demo/toggle`, sets cookie, revalidates, `demoMode` changes, nav re-renders.
- **Interactions:** Demo toggle button -> `POST /demo/toggle` -> cookie set -> revalidate -> `getAuthMe()` returns `demoMode: true` -> `getVisibleNavigation(true)`

### Test 14: Run detail page loads in production mode (browser)

- **Name:** Navigating to a run detail page in production mode renders without crashing, even when no runs exist
- **Type:** scenario
- **Disposition:** new
- **Harness:** Playwright browser harness
- **Preconditions:** Server running with in-memory store, no demo cookie, no runs created.
- **Actions:** Navigate to `/runs/nonexistent-id`.
- **Expected outcome:** Page renders (may show "Run not found" message). Does NOT crash with unhandled error or blank page. Source of truth: Plan Decision 6 -- run-detail loader uses `/runs/{id}` which returns 404, and component handles `run: null`.
- **Interactions:** Run detail loader -> `apiJson("/runs/nonexistent-id")` -> 404 -> graceful error handling

### Test 15: `mapRunSummaryToRunItem` correctly maps StoreRunSummary fields to RunItem

- **Name:** The mapping function converts server run summary response fields to the UI's RunItem shape
- **Type:** unit
- **Disposition:** new
- **Harness:** TypeScript unit test harness (bun test)
- **Preconditions:** None (pure function).
- **Actions:** Call `mapRunSummaryToRunItem()` with a complete `RunSummaryResponse` object containing `run_id`, `goal`, `workflow_slug`, `host_repo_path`, `duration_ms`.
- **Expected outcome:** Returns `RunItem` with `id` equal to `run_id`, `title` equal to `goal`, `workflow` equal to `workflow_slug`, `repo` equal to last segment of `host_repo_path`, `elapsed` formatted from `duration_ms`. Source of truth: Plan Decision 6 field mapping specification.
- **Interactions:** `formatElapsedSecs()` from `lib/format.ts`

### Test 16: `mapRunSummaryToRunItem` handles null optional fields

- **Name:** The mapping function provides sensible defaults when optional fields are null
- **Type:** boundary
- **Disposition:** new
- **Harness:** TypeScript unit test harness (bun test)
- **Preconditions:** None (pure function).
- **Actions:** Call `mapRunSummaryToRunItem()` with all optional fields set to null (`goal: null`, `workflow_slug: null`, `host_repo_path: null`, `duration_ms: null`).
- **Expected outcome:** Returns `RunItem` with `title: "Untitled run"`, `workflow: "unknown"`, `repo: "unknown"`, `elapsed: undefined`. Source of truth: Plan Task 4 step 3 default values.
- **Interactions:** None

### Test 17: `DemoModeProvider` provides demo mode value to children

- **Name:** The React context correctly propagates the demo mode boolean to consumer components
- **Type:** unit
- **Disposition:** new
- **Harness:** TypeScript unit test harness (bun test)
- **Preconditions:** None.
- **Actions:** Render `<DemoModeProvider value={true}><TestConsumer /></DemoModeProvider>` using `renderToString`. `TestConsumer` reads `useDemoMode()`.
- **Expected outcome:** Rendered HTML contains `data-demo="true"` and text "demo". Source of truth: Plan Decision 7 -- `DemoModeProvider` wraps `Outlet` with context value.
- **Interactions:** React context system

### Test 18: `getVisibleNavigation` filters demo-only items in production mode

- **Name:** The navigation filtering function excludes demo-only navigation items when not in demo mode
- **Type:** unit
- **Disposition:** new
- **Harness:** TypeScript unit test harness (bun test)
- **Preconditions:** None (pure function).
- **Actions:** Call `getVisibleNavigation(false)` and `getVisibleNavigation(true)`.
- **Expected outcome:** `getVisibleNavigation(false)` returns items that include "Runs" but NOT "Workflows" or "Insights". `getVisibleNavigation(true)` returns items that include "Runs", "Workflows", and "Insights". Source of truth: Plan Decision 4 -- Workflows and Insights are `demoOnly: true`.
- **Interactions:** None

---

## Coverage summary

### Covered areas

| Area | Tests | Coverage approach |
|---|---|---|
| Demo `/boards/runs` endpoint (new) | 1, 10 | Server integration + browser |
| Demo `GET /runs/{id}` shape fix | 2, 3 | Server integration |
| Real `/boards/runs` RunListItem enrichment | 4, 5, 6 | Server integration |
| Real `/boards/runs` board column mapping | 4, 5, 6 | Server integration |
| Run detail loader using `/runs/{id}` | 14, 15, 16 | Browser + unit |
| Demo mode toggle | 13 | Browser |
| Navigation filtering by demo mode | 8, 11, 18 | Browser + unit |
| DemoModeProvider context | 17 | Unit |
| mapRunSummaryToRunItem | 15, 16 | Unit |
| Runs board in production mode | 7 | Browser |
| Settings page in production mode | 9 | Browser |
| Workflows page in demo mode | 12 | Browser |

### Explicitly excluded (per agreed strategy)

| Area | Reason | Risk |
|---|---|---|
| Run stages tab rendering | Hidden in production mode; demo-only route. Unchanged code. | Low -- existing demo data path untouched. |
| Run files tab | Always hidden (endpoint does not exist). Tab removed from UI. | None. |
| Insights page deep interaction | Demo-only route. Unchanged code. Out of scope per "remove UI for removed functionality." | Low. |
| Workflow detail/diagram/runs pages | Demo-only routes. Static data, unchanged. | Low. |
| Run overview graph rendering (Graphviz) | Depends on `@viz-js/viz` WASM, hard to test in headless browser. Loader resilience tested via browser smoke. | Medium -- if WASM fails to load, graph won't render, but the page will still load with "No workflow graph available" fallback. |
| Run billing tab | Unchanged, already works in both modes. | None. |
| SSE event streaming | Unchanged, not part of this task. | None. |
| Performance benchmarks | Low performance risk -- no new hot paths, board query is bounded by in-memory run count. | Low. |
| Existing server test regressions from `/boards/runs` shape change | Existing tests that assert `status_reason`/`pending_control` from `/boards/runs` will need updating. Covered by implementation plan Task 3 Step 5, verified by running full `cargo nextest run -p fabro-server`. | Medium -- if missed, existing tests fail, caught immediately by CI. |
