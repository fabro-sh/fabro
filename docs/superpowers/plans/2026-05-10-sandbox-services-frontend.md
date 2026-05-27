# Sandbox Services Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Services tab to the sandbox page that lists backend-discovered services and opens Daytona preview URLs for supported ports.

**Architecture:** The sandbox page gains a new `services` mode between `terminal` and `filesystem`. A new SWR query calls the generated services endpoint, and a new panel renders the service list. Preview generation reuses the existing preview mutation semantics with signed URLs so browser opens do not require custom headers.

**Tech Stack:** React 19, React Router, SWR, generated TypeScript Axios client, Tailwind CSS, Heroicons.

---

## Scope

- Add a **Services** tab to `/runs/:id/sandbox?mode=services`.
- Place it between **Terminal** and **Filesystem**.
- List all services returned by the backend.
- Show a **Preview** action only for rows with `preview_supported: true`.
- Do not read repository setup metadata.
- Do not perform browser-side HTTP checks.
- Do not implement polling beyond normal SWR refresh/manual refresh behavior.

## Dependencies

- Requires the backend phase in `docs/superpowers/plans/2026-05-10-sandbox-services-backend.md`.
- Requires regenerated `@qltysh/fabro-api-client` types and `HumanInTheLoopApi.listSandboxServices`.

## Files

- Modify: `apps/fabro-web/app/lib/query-keys.ts`
- Modify: `apps/fabro-web/app/lib/queries.ts`
- Modify: `apps/fabro-web/app/lib/mutations.ts`
- Modify: `apps/fabro-web/app/routes/run-sandbox.tsx`
- Create: `apps/fabro-web/app/routes/run-sandbox/services-panel.tsx`
- Create: `apps/fabro-web/app/routes/run-sandbox/services-panel.test.tsx`
- Modify: `apps/fabro-web/app/routes/run-sandbox.test.tsx`

## Tasks

### Task 1: Add Client Query and Preview Support

- [ ] Add `queryKeys.runs.sandboxServices(id)` returning `["runs", "sandbox-services", id]`.
- [ ] Add `useSandboxServices(id)` in `queries.ts`:
  - SWR key is null when `id` is absent.
  - Fetcher calls `humanInTheLoopApi.listSandboxServices(id!)`.
  - Return type uses generated `SandboxServiceListResponse`.
  - Use `{ keepPreviousData: true }`.
- [ ] Extend `PreviewRunArg` in `mutations.ts` to include optional `signed?: boolean`.
- [ ] Ensure service preview calls pass `signed: true`.
- [ ] Keep the existing run detail Preview action working; its current `{ port: 3000, expires_in_secs: 3600 }` call remains valid.

### Task 2: Add Services Mode to the Sandbox Route

- [ ] Update `SandboxMode` to `"terminal" | "services" | "filesystem" | "vnc"`.
- [ ] Update `normalizeSandboxMode` so `"services"` is accepted and unknown values still default to `"terminal"`.
- [ ] Update `setMode` behavior:
  - `terminal` removes the `mode` query param.
  - `services`, `filesystem`, and `vnc` set `mode` to their mode name.
- [ ] Render tab order exactly as:
  - `Terminal`
  - `Services`
  - `Filesystem`
  - `VNC` when VNC is available.
- [ ] Render `<ServicesPanel runId={params.id} leading={modeToggle} />` when mode is `services`.
- [ ] Keep the existing VNC provider fallback behavior unchanged.

### Task 3: Build the Services Panel

- [ ] Create `services-panel.tsx`.
- [ ] Header:
  - left side renders `leading`.
  - right side has a refresh icon button using `ArrowPathIcon`.
  - refresh calls `servicesQuery.mutate()`.
- [ ] Body states:
  - loading: use existing `LoadingState`.
  - error: use existing `ErrorState` with the API error message when available.
  - empty: use `EmptyState` with title `No services`.
  - data: render a compact table inside a bordered panel.
- [ ] Table columns:
  - `Port`
  - `Bindings`
  - `Process`
  - `Action`
- [ ] Display rules:
  - Port is mono text.
  - Bindings joins `addresses` with `, `, or shows `-` when empty.
  - Process joins `processes` with `, `, or shows `-` when empty.
  - Previewable rows show a button labelled `Preview`.
  - Non-previewable rows show muted text `Unavailable`.
- [ ] Preview behavior:
  - Trigger `usePreviewRun(runId)` with `{ port, expires_in_secs: 3600, signed: true }`.
  - Open the returned URL in a new tab.
  - Disable the clicked preview button while the preview mutation is pending.
  - If preview fails, show an inline error state in the panel using `ErrorState` or a compact error banner.

### Task 4: Update Route Tests

- [ ] Update the query mock in `run-sandbox.test.tsx` to include `useSandboxServices`.
- [ ] Update default tab-count assertions:
  - Docker provider now has `Terminal`, `Services`, `Filesystem`.
  - Daytona provider now has `Terminal`, `Services`, `Filesystem`, `VNC`.
- [ ] Add tests:
  - `?mode=services` selects Services.
  - `normalizeSandboxMode("services")` returns `"services"`.
  - Docker `?mode=vnc` still falls back to Terminal.

### Task 5: Add Services Panel Tests

- [ ] Mock `useSandboxServices` and `usePreviewRun`.
- [ ] Test loading state.
- [ ] Test empty state.
- [ ] Test API error state.
- [ ] Test a non-previewable service on port `2500` is visible and has no Preview button.
- [ ] Test a previewable service on port `3000` renders Preview and triggers:
  - `port: 3000`
  - `expires_in_secs: 3600`
  - `signed: true`
- [ ] Test clicking Preview opens the returned URL in a new tab.

### Task 6: Verify

- [ ] Run `cd apps/fabro-web && bun test`.
- [ ] Run `cd apps/fabro-web && bun run typecheck`.
- [ ] Run `cd apps/fabro-web && bun run build`.
- [ ] If implementation changes layout materially, start the web dev flow and inspect `/runs/:id/sandbox?mode=services` in a browser.

## Acceptance Criteria

- The sandbox page includes a Services tab between Terminal and Filesystem.
- Services tab lists every service returned by the backend, including non-previewable ports.
- Only rows with `preview_supported: true` offer Preview.
- Preview opens a signed Daytona URL in a new browser tab.
- Existing Terminal, Filesystem, and VNC behavior remains intact.

## Assumptions

- Backend returns already-filtered `preview_supported` booleans; frontend does not recompute Daytona port ranges.
- Service discovery is manually refreshed by the user for v1.
- Table density should match the existing operational sandbox UI, not a marketing-style layout.
