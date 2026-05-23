# Automations End-to-End Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Automations as server-configured, API-manageable scheduled workflow definitions that persist to `settings.toml` and eventually create Fabro runs.

**Architecture:** Add a new `fabro-automation` crate for domain types, compact TOML parsing, schedule parsing, and structure-preserving TOML edits. Wire the crate through `fabro-config`, expose CRUD through `fabro-server`, store runtime execution state in SlateDB, and add a separate automation scheduler that creates regular Fabro runs.

**Tech Stack:** Rust, serde, toml/toml_edit, chrono, croner, Axum, OpenAPI/progenitor, SlateDB via `fabro-store`.

---

## Summary

Automations are defined in top-level TOML as `[automations.<id>]` and can be managed through REST endpoints. `settings.toml` remains the source of truth for automation definitions. Runtime execution state, including one-time consumed markers, lives in SlateDB so one-time automation definitions can remain enabled without firing repeatedly.

Definitions use compact TOML:

```toml
[automations.nightly-deps]
name = "Nightly dependency update"
repository = "fabro-sh/fabro"
ref = "main"
workflow = "dependency-update"

[automations.nightly-deps.trigger]
schedule = "0 3 * * *"
```

## Stage 1: `fabro-automation` Domain And TOML Config

- [ ] Create `lib/crates/fabro-automation` and add it to the workspace.
- [ ] Define strict domain types: `Automation`, `AutomationId`, `AutomationTarget`, `RepositorySlug`, `GitRefSelector`, `WorkflowSlug`, `AutomationTrigger`, and `ScheduleTrigger`.
- [ ] Add compact TOML-facing config types for `[automations.<id>]`.
- [ ] Default `enabled` to `true`.
- [ ] Parse `schedule = "now"` and RFC3339 timestamps as one-time schedules.
- [ ] Parse five-field cron strings as recurring schedules.
- [ ] Use UTC for cron evaluation in v1. Do not add timezone TOML yet.
- [ ] Reject event-based triggers explicitly for now.
- [ ] Use `croner` for cron parsing and next-occurrence calculation.
- [ ] Add tests for valid config, invalid IDs/slugs/repositories/refs, schedule classification, `now` normalization, and cron validation.

## Stage 2: Settings Integration

- [ ] Add top-level `automations` to `fabro-config`'s sparse settings layer using plural TOML: `[automations.<id>]`.
- [ ] Resolve automations into `ServerRuntimeSettings` so the server can load them alongside server settings.
- [ ] Keep `fabro-automation` independent of `fabro-server`; allow `fabro-config` to depend on `fabro-automation`.
- [ ] Add config tests proving `[automations.<id>]` parses from `settings.toml`, rejects malformed entries, and does not affect existing `[server]`, `[run]`, `[workflow]`, or `[cli]` behavior.
- [ ] Update server configuration docs with compact TOML examples.

## Stage 3: Structure-Preserving TOML Persistence

- [ ] Add a TOML edit module in `fabro-automation` built on `toml_edit::DocumentMut`.
- [ ] Provide operations for `list`, `get`, `create`, `replace`, `patch`, and `delete` under `automations.<id>`.
- [ ] Preserve unrelated comments, whitespace, table ordering, and non-automation sections.
- [ ] Update fields minimally when possible. Deleting an automation removes that automation table and its attached comments.
- [ ] Add config revision support using a stable hash of the current `settings.toml` contents.
- [ ] Implement server write flow: read file, parse with `toml_edit`, apply automation patch, validate full settings through `fabro-config`, write atomically under a file lock, then refresh in-memory runtime settings.
- [ ] Add golden tests with comments and odd spacing proving unrelated TOML is unchanged byte-for-byte where possible.

## Stage 4: REST API And OpenAPI

- [ ] Add REST endpoints under `/api/v1/automations`:

```http
GET    /api/v1/automations
POST   /api/v1/automations
GET    /api/v1/automations/{id}
PUT    /api/v1/automations/{id}
PATCH  /api/v1/automations/{id}
DELETE /api/v1/automations/{id}
```

- [ ] Make JSON create bodies include `id`.
- [ ] Make path-based `PUT` and `PATCH` bodies omit `id`, or reject bodies whose supplied `id` does not match the path.
- [ ] Require `If-Match` on mutating requests using the settings revision returned by GET/list responses.
- [ ] Return `409` for revision mismatch, `409` for duplicate create, `404` for missing automation, and `422` for domain validation errors.
- [ ] Add OpenAPI schemas and regenerate Rust and TypeScript API clients through the existing API workflow.
- [ ] Add server route tests for CRUD, conflict handling, validation errors, TOML persistence, and runtime settings reload.

## Stage 5: Automation Runtime State

- [ ] Add a SlateDB-backed automation state store in `fabro-store`.
- [ ] Track per automation: last attempted fire time, last successful fire time, last created run ID, last error, and one-time consumed marker.
- [ ] Keep one-time automation definitions enabled in `settings.toml`; use the consumed marker to prevent repeat execution.
- [ ] Expose status fields in `GET /api/v1/automations` and `GET /api/v1/automations/{id}` without writing status back to TOML.
- [ ] Add store tests for insert/update, one-time consumed behavior, status retrieval, and restart-safe persistence.

## Stage 6: Scheduler And Run Creation

- [ ] Add an automation scheduler service in `fabro-server` separate from the existing runnable-run scheduler.
- [ ] On startup and settings reload, evaluate enabled automations, compute due schedules, and create runs for due entries.
- [ ] Add server-side materialization for automation targets: clone or fetch the configured GitHub repo/ref into a temporary workspace, resolve the workflow slug with existing project workflow discovery rules, build a `RunManifest`, then reuse the existing run creation/start path.
- [ ] Set run provenance to identify the automation ID and system actor.
- [ ] Queue created runs immediately so the existing run scheduler executes them.
- [ ] Record automation success/failure in the automation state store.
- [ ] Add scheduler tests with frozen time for recurring schedules, `now`, fixed timestamps, disabled automations, one-time consumed behavior, failed materialization, and restart behavior.

## Stage 7: Docs And Rollout

- [ ] Document `[automations.<id>]` TOML, REST CRUD, optimistic concurrency, and runtime status.
- [ ] Document that v1 supports GitHub `owner/repo`, `ref` as a branch/tag/SHA selector, schedule triggers only, and UTC cron.
- [ ] Add an end-to-end integration test that creates an automation through the API, verifies `settings.toml` was minimally updated, reloads settings, fires the schedule, and observes a created run.

## Test Plan

- Config parsing tests in `fabro-automation` and `fabro-config`.
- TOML edit golden tests preserving comments, whitespace, and unrelated sections.
- API tests for all CRUD endpoints, validation failures, revision conflicts, and settings reload.
- Store tests for automation execution state and one-time consumed markers.
- Scheduler tests with controlled time for due/not-due schedules, disabled automations, one-time repeat prevention, and failed run materialization.
- OpenAPI conformance and generated-client checks after API schema updates.

## Assumptions And Defaults

- TOML root is plural: `[automations.<id>]`.
- `settings.toml` remains the source of truth for automation definitions.
- Runtime status and one-time consumed state live in SlateDB, not TOML.
- `ref = "main"` is accepted as a selector and resolved at run materialization time.
- Event triggers are out of scope for v1, but the type model should leave room for them.
- Cron schedules are UTC-only in v1.
- Writes require optimistic concurrency via `If-Match`.
- Before implementation, read `docs/internal/testing-strategy.md`, `docs/internal/error-handling-strategy.md`, and `docs/internal/logging-strategy.md` for the stages that add tests, API errors, and scheduler logging.
