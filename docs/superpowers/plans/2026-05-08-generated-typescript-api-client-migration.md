# Generated TypeScript API Client Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Make `apps/fabro-web` use the generated `@qltysh/fabro-api-client` Axios client for every API-style call that can reasonably be generated.

**Architecture:** Expand the OpenAPI spec for frontend-consumed API routes that are missing from the generated client, regenerate Rust and TypeScript clients, then add a web-side adapter around the generated Axios APIs. Migrate web queries and mutations from hard-coded request URLs to generated client methods while preserving existing SWR caching behavior, API error behavior, install flow behavior, and SSE streaming behavior.

**Tech Stack:** OpenAPI, Rust `fabro-api` progenitor generation, OpenAPI Generator `typescript-axios`, React 19, SWR, Axios, Bun, TypeScript.

---

## Summary

The repository already generates a TypeScript Axios client in `lib/packages/fabro-api-client`, but `apps/fabro-web` mostly imports generated model types while still sending requests through `fetch` helpers and hard-coded paths. This plan migrates the web app transport layer to generated API classes and expands OpenAPI for the frontend API routes that currently cannot be generated.

The migration is a transport refactor. It should not intentionally change server wire shapes, UI behavior, auth semantics, demo behavior, pagination limits, or SSE connection behavior.

## Important Interfaces And Contracts

- OpenAPI remains the source of truth at `docs/public/api-reference/fabro-api.yaml`.
- Generated TypeScript client output remains under `lib/packages/fabro-api-client/src/**`; do not hand-edit generated files.
- The web app should import generated API classes and types from `@qltysh/fabro-api-client`.
- Browser SSE remains custom `EventSource` code. The generated Axios client is not the transport for `/api/v1/attach` or `/api/v1/runs/{id}/attach`.
- Browser navigation and form endpoints remain browser routes:
  - `GET /auth/login/github`
  - `POST /auth/logout`

## Task 1: Add Missing Frontend API Routes To OpenAPI

**Files:**
- Modify: `docs/public/api-reference/fabro-api.yaml`
- Modify if needed: `lib/crates/fabro-server/src/web_auth.rs`
- Modify if needed: `lib/crates/fabro-server/src/server/handler/mod.rs`
- Test: `lib/crates/fabro-server/src/server/tests.rs` or existing OpenAPI conformance tests

- [x] Add `GET /api/v1/auth/config` with operation id `getAuthConfig`, tag `Auth`, and response schema matching the current UI expectation:
  - response object has `methods: string[]`
  - current values include auth method names such as `dev-token` and `github`

- [x] Add `GET /api/v1/auth/me` with operation id `getAuthMe`, tag `Auth`, and response schema matching the current `AuthMeResponse` in `web_auth.rs`:
  - `user.login`
  - `user.name`
  - `user.email`
  - optional `user.idpIssuer`
  - optional `user.idpSubject`
  - `user.avatarUrl`
  - `user.userUrl`
  - `provider`
  - `demoMode`

- [x] Add `POST /api/v1/demo/toggle` with operation id `toggleDemo`, tag `Auth`, request body `{ enabled: boolean }`, and response body `{ enabled: boolean }`.

- [x] Add `POST /auth/login/dev-token` with operation id `loginDevToken`, tag `Auth`, request body `{ token: string }`, and response body `{ ok: boolean }`.

- [x] Add workflow routes consumed by the demo-only workflow pages:
  - `GET /api/v1/workflows` with operation id `listWorkflows`
  - `GET /api/v1/workflows/{name}` with operation id `retrieveWorkflow`
  - `GET /api/v1/workflows/{name}/runs` with operation id `listWorkflowRuns`

- [x] Define workflow schemas from the current hand-written TypeScript shapes in `apps/fabro-web/app/lib/workflow-api.ts`:
  - `WorkflowScheduleSummary`
  - `WorkflowLastRunSummary`
  - `WorkflowListItem`
  - `PaginatedWorkflowListResponse`
  - `WorkflowDetailResponse`

- [x] Preserve current server behavior for workflow routes:
  - real routes may continue returning `501 Not Implemented`
  - demo routes return demo workflow data
  - OpenAPI should document normal success responses plus standard API error responses

- [x] Fix existing spec drift for `GET /api/v1/runs/{id}/graph`:
  - add optional query parameter `direction`
  - allowed values: `LR`, `TB`, `BT`, `RL`
  - generated `RunsApi.retrieveRunGraph` should accept the direction argument after regeneration

- [x] Confirm no OpenAPI additions are needed for recently added run deletion and artifacts UI calls.
  - `RunsApi.deleteRun` is already generated from `DELETE /api/v1/runs/{id}`.
  - `RunInternalsApi.listRunArtifacts` is already generated from `GET /api/v1/runs/{id}/artifacts`.
  - `RunInternalsApi.getStageArtifact` is already generated from `GET /api/v1/runs/{id}/stages/{stageId}/artifacts/download`.

## Task 2: Regenerate API Clients

**Files:**
- Modify generated: `lib/crates/fabro-api/src/**`
- Modify generated: `lib/packages/fabro-api-client/src/**`
- Test: `lib/packages/fabro-api-client`

- [x] Run `cargo build -p fabro-api`.
  - Expected: Rust OpenAPI client/types regenerate and compile.

- [x] Run `cd lib/packages/fabro-api-client && bun run generate`.
  - Expected: TypeScript Axios client regenerates with Auth and Workflow APIs plus updated `retrieveRunGraph` signature.

- [x] Run `cd lib/packages/fabro-api-client && bun run typecheck`.
  - Expected: generated client typechecks.

- [x] Inspect generated `lib/packages/fabro-api-client/src/api.ts`.
  - Expected: exports include generated APIs for auth/workflow routes, either as new tag APIs or existing tag APIs depending on the chosen OpenAPI tags.
  - Expected: existing exports still expose `RunsApi.deleteRun`, `RunInternalsApi.listRunArtifacts`, and `RunInternalsApi.getStageArtifact`.

## Task 3: Add A Web-Side Generated Client Adapter

**Files:**
- Modify or split: `apps/fabro-web/app/lib/api-client.ts`
- Possibly create: `apps/fabro-web/app/lib/generated-api.ts`
- Modify: `apps/fabro-web/package.json`
- Test: `apps/fabro-web/app/lib/api-client.test.ts`

- [x] Add `axios` as an explicit dependency of `apps/fabro-web`.
  - Rationale: `@qltysh/fabro-api-client` already depends on Axios, but the web app should explicitly own the runtime transport it configures.

- [x] Create a single generated API adapter module that:
  - creates one Axios instance with same-origin behavior and credentials included
  - creates `Configuration({ basePath: "" })`
  - exports instantiated generated API classes used by the web app
  - exposes helpers for converting Axios responses to response data

- [x] Preserve the existing `ApiError` contract used by UI components:
  - `status`
  - `requestId`
  - `body`
  - message behavior compatible with current callers

- [x] Normalize generated Axios failures into `ApiError`.
  - Extract request id from response headers first, then response body.
  - Preserve existing body parsing for JSON API error responses.
  - Preserve current 401 redirect to `/login` for normal authenticated app API calls.

- [x] Add a no-redirect mode for install and login calls.
  - Install requests and dev-token login failures must reject with an error the install/login UI can display, not redirect to `/login`.

- [x] Keep text/blob response support for generated calls that return non-JSON data:
  - graph SVG
  - graph DOT source
  - run logs
  - stage artifact downloads
  - stage command logs where generated response is typed as JSON

- [x] Add an artifact-download helper that uses generated client metadata instead of hand-built route strings.
  - Preferred shape: use the generated `RunInternalsApi` operation or generated request builder for `getStageArtifact`.
  - Preserve browser download behavior in `run-artifacts.tsx`; if using an `<a href>`, build that href through the generated request builder rather than duplicating the endpoint path.

## Task 4: Migrate Install, Auth, Mutations, And Run Actions

**Files:**
- Modify: `apps/fabro-web/app/install-api.ts`
- Modify: `apps/fabro-web/app/lib/mutations.ts`
- Modify: `apps/fabro-web/app/lib/run-actions.ts`
- Test: `apps/fabro-web/app/install-api.test.ts`
- Test: `apps/fabro-web/app/lib/run-actions.test.ts`
- Test: mutation-related component tests as needed

- [x] Replace `install-api.ts` custom `fetch` calls with generated `InstallApi` methods.
  - Pass the one-time install token through `Authorization: Bearer <token>` request options.
  - Keep the existing exported function names so `install-app.tsx` does not need broad rewiring.
  - Keep existing install error messages from API error details where available.

- [x] Replace `useLoginDevToken` custom `fetch` with generated `loginDevToken`.
  - Preserve `credentials: include` behavior through the Axios instance.
  - Preserve UI behavior: failed login shows "Invalid dev token."

- [x] Replace generated-capable mutations in `mutations.ts`:
  - preview uses `HumanInTheLoopApi.generatePreviewUrl`
  - submit interview answer uses `HumanInTheLoopApi.submitRunAnswer`
  - interrupt uses `HumanInTheLoopApi.interruptRun`
  - steer uses `HumanInTheLoopApi.steerRun`
  - demo toggle uses generated `toggleDemo`

- [x] Replace lifecycle actions in `run-actions.ts`:
  - cancel uses `RunsApi.cancelRun`
  - archive uses `RunsApi.archiveRun`
  - unarchive uses `RunsApi.unarchiveRun`
  - delete uses `RunsApi.deleteRun`

- [x] Preserve lifecycle error mapping.
  - `mapError` should still receive `LifecycleActionError` with `status` and `errors`.
  - Existing 404/409 UI messages should remain unchanged.

- [x] Preserve archived-run delete behavior.
  - `deleteRun` should still treat `204` and `404` as success.
  - `deleteErrorMessage` should still map `409` to "Active runs can't be deleted."
  - `run-detail.tsx` should still invalidate both normal and archived board keys and navigate back to `/runs` after successful deletion.

## Task 5: Migrate Queries And Cache Keys

**Files:**
- Modify: `apps/fabro-web/app/lib/queries.ts`
- Modify: `apps/fabro-web/app/lib/query-keys.ts`
- Modify: `apps/fabro-web/app/lib/board-events.ts`
- Modify: `apps/fabro-web/app/lib/run-events.ts`
- Modify: `apps/fabro-web/app/lib/cross-tab-sse.ts`
- Modify: `apps/fabro-web/app/routes/run-artifacts.tsx`
- Test: `apps/fabro-web/app/lib/query-keys.test.ts`
- Test: `apps/fabro-web/app/lib/board-events.test.tsx`
- Test: `apps/fabro-web/app/lib/run-events.test.tsx`
- Test: `apps/fabro-web/app/lib/cross-tab-sse.test.ts`

- [x] Change SWR cache keys from request URLs to semantic tuple keys.
  - Example shape: `["runs", "detail", id]`, `["runs", "files", id]`, `["board-runs", includeArchived]`.
  - Keep one source of truth in `query-keys.ts`.

- [x] Keep dedicated URL builders only for SSE streams.
  - `queryKeys.system.attachUrl()` or equivalent for `/api/v1/attach`
  - `queryKeys.runs.attachUrl(id)` or equivalent for `/api/v1/runs/{id}/attach`
  - Do not keep hand-built URL helpers for artifact downloads; route construction should come from generated client metadata.

- [x] Migrate `queries.ts` hooks to generated APIs:
  - auth config and current user
  - system info
  - board runs with pagination
  - run detail, state, files, stages, graph, graph source, logs, artifacts, settings, billing, questions
  - stage events with cursor pagination
  - stage command log
  - stage artifact download support for `run-artifacts.tsx`
  - workflows, workflow detail, workflow runs
  - insights queries and history
  - server settings

- [x] Preserve current pagination safety caps:
  - normal page-based fetches keep max page and max item caps
  - stage events keep cursor pagination from `since_seq=1`, page size `1000`, and non-advancing/empty-page guards

- [x] Update board/run SSE invalidation to emit semantic cache keys.
  - Existing invalidation behavior should remain the same.
  - Terminal run events should still refresh detail, files, artifacts, billing, stages, and graph keys.
  - Checkpoint/artifact-producing events should refresh the artifacts key when the current event taxonomy supports that distinction; otherwise terminal refresh coverage is acceptable for this migration.
  - Stage activity events should still refresh only the selected stage event key.

- [x] Update tests that assert URL cache keys to assert semantic cache keys or explicit SSE URLs.

## Task 6: Remove Dead Custom Transport And Verify

**Files:**
- Modify: `apps/fabro-web/app/lib/api-client.ts`
- Modify tests that mock `globalThis.fetch`
- Test: `apps/fabro-web`

- [x] Remove or reduce custom `fetch` helpers that are no longer used:
  - `apiRequest`
  - `apiFetcher`
  - `apiNullableFetcher`
  - `apiTextFetcher`
  - `apiNullableTextFetcher`
  - `apiJsonMutation`
  - Keep only compatibility helpers still needed by the new generated adapter.

- [x] Update tests that mock `globalThis.fetch`.
  - Generated-client tests should mock the adapter or Axios instance, not browser fetch.
  - SSE tests can continue mocking `EventSourceLike`.

- [x] Run `cd apps/fabro-web && bun run typecheck`.
  - Expected: web app typechecks against generated API client method signatures.

- [x] Run `cd apps/fabro-web && bun test`.
  - Expected: all web tests pass after cache-key and transport test updates.

- [x] Run `cargo nextest run -p fabro-server`.
  - Expected: OpenAPI/router behavior remains valid for server routes.

- [x] Final inventory check:
  - `rg -n "\\bfetch\\(|/api/v1/|apiFetcher|apiJsonMutation|apiRequest" apps/fabro-web/app -g '*.ts' -g '*.tsx'`
  - Expected: remaining hits are tests, SSE URL builders, browser navigation/form routes, comments, or documented exceptions.

## Explicit Exceptions

- Keep `EventSource` for SSE:
  - `/api/v1/attach`
  - `/api/v1/runs/{id}/attach`
- Keep browser navigation/form routes:
  - `/auth/login/github`
  - `/auth/logout`
- Do not attempt to replace static asset or browser route links with generated API calls.

## Acceptance Criteria

- All frontend API-style JSON/text requests that have generated operations use `@qltysh/fabro-api-client` API classes.
- Run artifacts list/download and archived-run deletion use generated client operations or generated request builders.
- Frontend-consumed API routes missing from the client are added to OpenAPI and generated.
- No generated TypeScript files are hand-edited.
- Existing UI behavior is preserved for auth, install, demo toggle, lifecycle actions, run detail views, workflows, insights, and settings.
- SSE remains browser-native and continues to coordinate cross-tab subscriptions.
- TypeScript, generated-client, and server tests listed above pass.

## Assumptions

- Expanding OpenAPI is desired for frontend API-style routes, including auth config/me, demo toggle, dev-token login, and workflow pages.
- Workflow routes can be generated even though real mode currently returns `501`; generated success types should model the demo/expected response shape.
- This work does not implement real workflow routes or insights routes; it only types and consumes their existing API contracts.
- This work does not change public wire shapes except for documenting routes already served by the server and adding the missing graph `direction` query parameter to the spec.
