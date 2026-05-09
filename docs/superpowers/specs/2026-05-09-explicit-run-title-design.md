# Explicit Event-Sourced Run Titles

## Summary

Add an explicit run `title` that is always a non-blank string in API responses. On creation, clients may provide `title`; otherwise the server infers it from the run goal exactly once for new runs. Later title changes are persisted through a new event, not by mutating stored summaries or recomputing from the goal.

## Key Changes

- Add optional `title` to `RunManifest`; validate provided titles by trimming, rejecting blank/whitespace-only values, and rejecting values over 100 chars.
- Add shared title helpers:
  - explicit title normalization: trim, require non-blank, max 100, reject invalid input.
  - inferred title: current first-line goal cleanup, truncate to 100, fallback to `"Untitled run"` if inference is blank.
- Store resolved title on new `run.created` events. Keep legacy event replay compatible by inferring title from goal only when replaying old `run.created` events that lack a title.
- Add `run.title.updated` with `{ "title": "..." }`; update `RunProjection.title`, and make `RunSummary.title` read from projection title instead of deriving from `goal`.
- Add `PATCH /api/v1/runs/{id}` with body `{ "title": "..." }`; return updated `RunSummary`.
- Allow title PATCH for all run states, including archived runs, as a metadata-only exception to archived read-only behavior.
- If PATCH normalizes to the existing title, return the current summary without appending a no-op event.
- No web edit UI in this slice. Do update frontend/SSE invalidation so `run.title.updated` refreshes run detail and board data when another client changes a title.

## Public Interfaces

- OpenAPI:
  - `RunManifest.title?: string | null`
  - new `UpdateRunRequest` with required `title: string`
  - `PATCH /api/v1/runs/{id}` returns `RunSummary`
  - document `RunSummary.title` and `RunListItem.title` as non-blank strings
- Generated clients:
  - rebuild Rust API types and TypeScript API client after OpenAPI changes.
- CLI:
  - no new `fabro run --title` flag in this plan; CLI-created runs continue relying on server-side inference unless a caller builds a manifest with `title`.

## Test Plan

- Unit tests for title normalization: trims, rejects blank, rejects over 100, preserves valid title.
- Unit tests for inference: strips markdown heading and `Plan:`, truncates to 100, returns `"Untitled run"` for blank/empty goals.
- Store/projection tests:
  - new `run.created` with title populates projection and summary title.
  - old `run.created` without title still replays and yields inferred/fallback title.
  - `run.title.updated` changes projection and summary.
- Server API tests:
  - create run with explicit title returns that title.
  - create run without title returns inferred title.
  - blank/whitespace create title returns 400.
  - PATCH updates active, terminal, and archived runs.
  - PATCH rejects blank and over-100 titles with 400.
  - same-title PATCH is idempotent and does not append an update event.
- Frontend tests:
  - board and run-detail invalidation include `run.title.updated`.
  - existing rendering continues to display server `title`.

## Assumptions

- Maximum explicit title length is 100 characters.
- Explicit titles are rejected when invalid; only inferred titles are truncated.
- `"Untitled run"` is the canonical server fallback when inference cannot produce a non-blank title.
- Existing legacy runs may still have their title inferred during replay because old event streams did not record a resolved title.
