# Unified Run Type Shape Plan

## Summary

Refactor the public run API around one canonical `Run` shape for lists, detail pages, board data, create/update responses, and future filtering. This is a breaking greenfield cleanup: remove legacy parallel DTOs instead of carrying aliases.

The public shape should preserve valid distinctions:

- `RunSandbox` is durable/static run-owned sandbox metadata.
- `SandboxDetails` is live provider data.
- `PullRequest` is durable/static PR metadata.
- `PullRequestDetails` is live GitHub-enriched data.

No server-side filter query params are added in this pass; this pass makes the data model filter-ready. This is also a domain-model cleanup: internal lifecycle semantics and public lifecycle semantics should align where possible.

## Key Changes

Replace public `RunSummary`, `RunListItem`, and `RunStatusResponse` with one `Run` schema:

```ts
type Run = {
  id: string
  title: string
  goal: string
  workflow: WorkflowRef
  automation: AutomationRef | null
  repository: RepositoryRef | null
  created_by: Principal | null
  origin: RunOrigin
  labels: Record<string, string>
  lifecycle: RunLifecycle
  sandbox: RunSandbox | null
  models: RunModel[]
  source_directory: string | null
  timestamps: RunTimestamps
  billing: RunBillingSummary | null
  diff: DiffSummary | null
  pull_request: PullRequest | null
  current_question: RunQuestion | null
  superseded_by: string | null
  links: { web: string | null }
}
```

Use these supporting public types:

```ts
type WorkflowRef = { slug: string | null; name: string }
type AutomationRef = { id: string; name: string | null }
type RepositoryRef = { name: string; origin_url: string | null; provider: "github" | "git" | "unknown" }
type RunOrigin = { kind: "api" }
type RunModel = { provider: string | null; name: string }
type RunTimestamps = { created_at: string; started_at: string | null; last_event_at: string | null; completed_at: string | null }
type RunBillingSummary = { total_usd_micros: number | null }
```

Modify `RunStatus` so archive is not a status. Keep the existing status payloads that carry product meaning:

```ts
type RunLifecycle = {
  status: RunStatus
  pending_control: RunControlAction | null
  queue_position: number | null
  error: RunError | null
  archived: boolean
  archived_at: string | null
}

type RunStatus =
  | { kind: "submitted" }
  | { kind: "runnable" }
  | { kind: "starting" }
  | { kind: "running" }
  | { kind: "blocked"; blocked_reason: BlockedReason }
  | { kind: "paused"; prior_block: BlockedReason | null }
  | { kind: "removing" }
  | { kind: "succeeded"; reason: SuccessReason }
  | { kind: "failed"; reason: FailureReason }
  | { kind: "dead" }
```

Use this sandbox split:

```ts
type RunSandbox = {
  provider: SandboxProvider
  image: string | null
  snapshot: string | null
  runtime: {
    id: string
    working_directory: string
    repo_cloned: boolean | null
    clone_origin_url: string | null
    clone_branch: string | null
  } | null
}

type SandboxDetails = {
  sandbox: RunSandbox
  state: SandboxState
  native_state: string | null
  region: string | null
  resources: SandboxResources
  labels: Record<string, string>
  timestamps: SandboxTimestamps
}
```

Use this pull request split:

```ts
type PullRequest = {
  provider: "github"
  owner: string
  repo: string
  number: number
  html_url: string
  title: string
  base_branch: string
  head_branch: string
}

type PullRequestDetails = {
  pull_request: PullRequest
  state: string
  draft: boolean
  merged: boolean
  merged_at: string | null
  mergeable: boolean | null
  additions: number
  deletions: number
  changed_files: number
  comments: number
  checks: CheckRun[]
  author: { login: string }
  timestamps: { created_at: string; updated_at: string }
}
```

Use these derivation rules:

- Board column:
  - `run.lifecycle.archived` -> `archived`
  - `submitted` / `runnable` -> `runnable`
  - `starting` -> `initializing`
  - `running` / `paused` -> `running`
  - `blocked` -> `blocked`
  - `succeeded` -> `succeeded`
  - `failed` / `dead` -> `failed`
  - `removing` -> omitted from the board response unless a removal column is added intentionally
- `RepositoryRef.provider`:
  - GitHub HTTPS and SSH origins -> `github`
  - any other non-empty Git origin URL -> `git`
  - missing or unparseable origin -> `unknown`
- `RepositoryRef.name`:
  - GitHub origins use `owner/repo`
  - other Git origins use the best available repo basename
  - missing origins fall back to source directory basename, then `unknown`
- `RunOrigin`:
  - API-created runs use `{ kind: "api" }`
  - no structured origin header or user-agent-derived public origin is added in this pass

## Implementation Changes

- In `fabro-types`, add canonical public run types and remove/rename redundant public DTOs:
  - `RunSummary` -> `Run`
  - `RepositoryReference` -> `RepositoryRef`
  - `PullRequestRecord` -> `PullRequest`
  - `PullRequestDetail` -> `PullRequestDetails`
  - remove `RunListItem`, board-only `RunPullRequest`, and public `RunStatusResponse`
- Modify `RunStatus` as the shared domain/public execution status:
  - remove `Archived { prior }`
  - preserve payloads on `Blocked`, `Paused`, `Succeeded`, and `Failed`
  - update helpers such as `is_terminal`, `is_immutable`, `is_active`, and `can_transition_to`
- Add archive metadata to `RunProjection`:
  - `status: RunStatus`
  - `archived_at: Option<DateTime<Utc>>`
  - archive/unarchive operations set or clear archive metadata instead of transitioning status
  - archived runs remain read-only because archive metadata is present, not because status is special
- Replace `RunProvenance` as a public filtering source with first-class fields:
  - `created_by: Principal | null`
  - `origin: RunOrigin`
  - Direct API requests default to `origin.kind = "api"`.
  - No structured origin header or user-agent-derived public origin is added in this pass.
- Update projection reduction so `run.created` initializes durable filter metadata:
  - workflow, repository, labels, creator, origin, source directory
  - sandbox provider/image/snapshot from resolved run settings
  - archive metadata as `archived_at`, not `RunStatus::Archived`
- Update `run.archived` and `run.unarchived` reduction:
  - `run.archived` requires a terminal status and sets `archived_at`
  - `run.unarchived` clears `archived_at`
  - neither event changes `status`
- Update `sandbox.initialized` handling to fill `RunSandbox.runtime`.
- Update model aggregation in the run builder:
  - collect observed stage models from `StageProjection`
  - dedupe by `(provider, name)`
  - sort deterministically
- Change public endpoints to return `Run`:
  - `GET /api/v1/runs`
  - `GET /api/v1/runs/{id}`
  - `GET /api/v1/runs/resolve`
  - `GET /api/v1/boards/runs`
  - `POST /api/v1/runs`
  - `PATCH /api/v1/runs/{id}`
  - `POST /api/v1/runs/{id}/cancel`
  - `POST /api/v1/runs/{id}/start`
  - `POST /api/v1/runs/{id}/pause`
  - `POST /api/v1/runs/{id}/unpause`
  - `POST /api/v1/runs/{id}/archive`
  - `POST /api/v1/runs/{id}/unarchive`
- Keep `/api/v1/runs/{id}/state` as the internal event-sourced projection endpoint; update it only as needed for renamed internal fields.
- Update the web app to consume `Run` directly:
  - board column is derived from `run.lifecycle.status` and `run.lifecycle.archived`
  - archived column uses `run.lifecycle.archived`
  - archive actions use archive metadata (`archived` / `archived_at`), not status kind
  - cancelled-run detection uses `run.lifecycle.status.kind === "failed"` and `reason === "cancelled"`
  - filters/read models use paths like `workflow.slug`, `repository.name`, `origin.kind`, `sandbox.provider`, `sandbox.image`, `models[].name`.
- Update CLI consumers of run status:
  - `fabro ps`
  - `fabro archive`
  - `fabro unarchive`
  - `fabro rewind`
  - output formatting that previously checked `RunStatus::Archived`
- Update documentation and generated clients:
  - OpenAPI schemas remove `RunStatusArchived`, `TerminalStatus`, `RunSummary`, `RunListItem`, and `RunStatusResponse`
  - regenerate the TypeScript API client
  - update public docs that describe `archived` as a status

## Test Plan

- Update OpenAPI round-trip tests to assert `fabro_api::types::Run` reuses `fabro_types::Run`.
- Update OpenAPI round-trip tests to assert `fabro_api::types::RunStatus` reuses `fabro_types::RunStatus`.
- Add JSON shape tests for:
  - workflow/repository nesting
  - `created_by` and `origin`
  - sandbox planned metadata before runtime initialization
  - sandbox runtime after `sandbox.initialized`
  - multiple deduped models
  - archived succeeded run with `lifecycle.archived = true` and `lifecycle.status.kind = "succeeded"`
  - archived failed-cancelled run preserving `lifecycle.status.reason = "cancelled"`
  - unarchive clearing `archived_at` without changing status
  - archive filtering and mutation rejection using archive metadata
  - static `PullRequest` and live `PullRequestDetails`
- Update server API tests for list/detail/board/create/update/lifecycle responses.
- Update web tests for board mapping, run detail mapping, archive behavior, cancelled-run behavior, and sandbox display.
- Update CLI tests that assert archived status text or archive/unarchive behavior.
- Run:
  - `cargo nextest run -p fabro-types -p fabro-store -p fabro-workflow -p fabro-api -p fabro-server -p fabro-cli`
  - `cd apps/fabro-web && bun test`
  - `cd apps/fabro-web && bun run typecheck`

## Assumptions

- Breaking API cleanup is allowed; no legacy field aliases are kept.
- This pass does not add server-side filter query params.
- Existing historical run-event/projection/API compatibility is not required for this greenfield cleanup.
- Do not keep migration shims, deprecated schemas, serde aliases, or compatibility adapters for removed run DTOs/status variants.
- Live external data stays out of list filtering; filterable run data must come from durable `Run`.
