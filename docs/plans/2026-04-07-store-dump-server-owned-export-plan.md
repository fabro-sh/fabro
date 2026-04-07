# Store Dump Server-Owned Export Plan

## Summary

Align `fabro store dump` with the intended architecture by making it a pure server-backed export command.

- The CLI should resolve runs from server summaries only.
- It should fetch hydrated run state, hydrated event history, and artifacts over HTTP.
- It should stop creating a temporary `Database`.
- It should stop reading local scratch directories or local storage/object-store paths.
- It can keep the current export-style output layout on disk, except blob storage should stay transparent:
  - top-level metadata files
  - `nodes/**`
  - `retro/**`
  - `events.jsonl`
  - `checkpoints/**`
  - `artifacts/**`

This is a debugging export, not a strict snapshot mechanism. Best-effort consistency is acceptable.

## Problem Frame

`fabro store dump` is still implemented as a local store reconstruction flow:

- it resolves the run through local-storage-aware helpers
- it fetches events over HTTP but replays them into an in-memory `fabro_store::Database`
- it reads artifacts from local storage directly
- it assumes the CLI can see the same storage and scratch directories as the server

That is now the wrong boundary. The server should own store access; the CLI should only export server-provided run data to disk.

## Key Decisions

- `store dump` becomes server-only.
  - Use `ServerTargetArgs`, not `StorageDirArgs`.
  - Resolve selectors through `ServerSummaryLookup`, not `ServerRunLookup`.
  - Do not scan local scratch directories or orphan runs.

- Keep the current export layout.
  - It is fine that the command still writes `events.jsonl`, `checkpoints/`, and `artifacts/`.
  - It should not write a `blobs/` directory.
  - Blob storage is an internal offload mechanism, not part of the user-facing export model.

- Do not add blob enumeration.
  - Blob-backed values should be hydrated server-side before the CLI receives run state or events.
  - The CLI should not need to understand blob pointer conventions.
  - The exported files should look as if offloading had never happened.

- Best-effort race handling is acceptable.
  - If a blob or artifact is listed/referenced but disappears before download, skip it and continue.
  - Fail on transport or server errors other than `404`.

- Event pagination should use the server's `meta.has_more` contract.
  - Do not stop paging based on "returned fewer than page size".
  - The client should retain and use pagination metadata from the API response.

- Artifact downloads should be concurrent with a fixed upper bound.
  - Use a bounded concurrency strategy such as `JoinSet` or `FuturesUnordered` with a small limit.
  - Recommended default: `8` concurrent artifact downloads.

## Implementation Changes

### 1. Convert `store dump` to standard server targeting

In `fabro-cli`:

- change `StoreDumpArgs` to flatten `ServerTargetArgs`
- remove `StorageDirArgs` from this command
- update help text, docs, and snapshots to show `--server` / `FABRO_SERVER`

Run resolution should follow the same contract as other server-backed inspection commands:

1. explicit `--server`
2. configured `[server].target`
3. default local server instance if no server target is configured

The command should no longer derive behavior from a local storage dir.

### 2. Resolve the run from server summaries only

- replace `ServerRunLookup` usage with `ServerSummaryLookup`
- resolve `<RUN>` from server-provided summaries only
- remove any dependence on local scratch-path scanning during selector resolution

This ensures `store dump` works even when the server runs on a different host.

### 3. Add server/API support for paginated hydrated reads

The CLI path depends on the server returning enough information to page correctly and to keep blob handling transparent.

Add or update the server/API contract for:

- `GET /runs/{id}/state?hydrate_blobs=true`
- `GET /runs/{id}/events?...&hydrate_blobs=true`

Follow the existing OpenAPI-first workflow for these API changes:

- update `docs/api-reference/fabro-api.yaml`
- rebuild Rust API types/client via `cargo build -p fabro-api`
- regenerate the TypeScript client in `lib/packages/fabro-api-client`

Also update the CLI client shape for event listing so pagination metadata is preserved instead of discarded:

- add a new paginated event-list helper for `store dump` that returns both `data` and `meta.has_more`
- keep the existing `list_run_events(...) -> Vec<EventEnvelope>` convenience method in place for current callers unless there is a strong reason to migrate them in the same change
- `store dump` must consume the metadata-bearing form

The existing artifact APIs are already sufficient for this plan:

- `list_run_artifacts(run_id)`
- `download_stage_artifact(run_id, stage_id, filename)`

No new artifact endpoint work is required here.

### 4. Add server-side blob hydration for state and events

Blob storage is intended to be transparent. `store dump` should not detect, enumerate, or hydrate blob pointers in the CLI.

Recommended API shape:

- add `hydrate_blobs=true` as an optional query parameter on both endpoints
- when omitted or `false`, preserve current behavior
- when `true`, the server resolves blob-backed references before serializing the response
- reflect those query parameters in the OpenAPI schema and generated clients

Hydration scope:

- all blob-backed values inside `RunProjection`
- all blob-backed values inside returned event payloads
- this includes values that ultimately flow into exported checkpoint JSON because checkpoints come from `RunProjection.checkpoints`

Hydration mechanism:

- implement a shared server-side JSON hydrator that walks `serde_json::Value`, detects blob-backed pointer values, reads the referenced blobs from the run store, and replaces the pointer string with the parsed JSON payload
- reuse the same helper for both state and event responses so blob resolution rules stay identical
- keep the helper server-owned rather than attaching it only to `RunProjection`, because event payloads need the same treatment

If a referenced blob cannot be resolved during hydrated fetch:

- treat `404` as a race-tolerant miss
- preserve the original pointer value in the hydrated response
- fail on non-`404` server/store errors

This keeps blob knowledge server-owned, which matches the architecture this plan is trying to enforce.

### 5. Refactor `RunDump` to accept fetched export data instead of store handles

`RunDump::store_export` currently assumes:

- a `RunDatabase`
- an `ArtifactStore`
- store enumeration for blobs and artifacts

Refactor this into a store-agnostic export builder with a concrete constructor:

```rust
RunDump::from_export(
    state: &RunProjection,
    events: &[EventEnvelope],
    artifacts: &HashMap<(StageId, String), Bytes>,
) -> Result<Self>
```

Notes:

- `state` is already hydrated
- `events` are already hydrated
- blobs are not a separate constructor parameter
- `artifacts` are the only binary payloads the CLI still fetches explicitly

The new builder should preserve the current file layout and validation rules:

- top-level metadata files from `RunProjection`
- node files under `nodes/<node>/visit-<n>/`
- `retro/prompt.md` and `retro/response.md`
- `events.jsonl`
- `checkpoints/<seq>.json` sourced directly from `RunProjection.checkpoints`
- `artifacts/nodes/<node>/visit-<n>/<relative_path>`

Keep the existing staged-directory write behavior:

- reject non-empty output dirs
- write into a temp dir under the output parent
- rename into place when complete

### 6. Replace local store reconstruction with HTTP-backed export collection

In `dump_command`, fetch the export inputs directly from the server:

- hydrated current run state via `get_run_state(run_id, hydrate_blobs = true)`
- full hydrated event history via paginated `list_run_events(run_id, since_seq, limit, hydrate_blobs = true)`
  - use an explicit page size of `1000`
  - continue paging based on `meta.has_more`
- run artifacts via `list_run_artifacts(run_id)`
- artifact contents via `download_stage_artifact(run_id, stage_id, filename)` using bounded concurrency

Do not:

- create a `Database`
- call `rebuild_run_store`
- open a local `ArtifactStore`
- read anything under local `storage/` or scratch paths

### 7. Leave shared event-rebuild helpers for other commands

`rebuild_run_store` is still used by other commands such as `fork`, `rewind`, and `pr create`.

- remove it from `store dump`
- do not delete it in this change unless those other commands are migrated too

This plan is specific to aligning `store dump` with server-owned export.

## Test Plan

- update the `store dump --help` snapshot for the CLI arg change from `--storage-dir` to `--server`
- re-enable the disabled `store dump` integration coverage and make it exercise the server-backed path only
- add a regression test for a run with more than 100 events to prove pagination exports the full `events.jsonl`
- add a regression test for a run with exactly `1000` events to prove the client performs the additional page fetch and stops on `has_more = false`, not on page-size heuristics
- add server/API coverage for hydrated fetches:
  - hydrated `get_run_state(..., hydrate_blobs = true)` replaces blob-backed pointers with original JSON values
  - hydrated `list_run_events(..., hydrate_blobs = true)` replaces blob-backed pointers in event payloads
  - blob `404` during hydration preserves the original pointer value
- add coverage that no `blobs/` directory is emitted
- add `RunDump` coverage for the new store-agnostic constructor to verify the exported file layout remains unchanged for representative state/events/blobs/artifacts
- keep or restore the non-empty output-dir rejection test
- add a race-tolerance test where a referenced artifact returns `404` and the export completes without that artifact file
- add coverage that artifact downloads run through the bounded-concurrency path without changing output order or file paths

## Assumptions and Defaults

- `store dump` should use the standard server-target contract, not `--storage-dir`
- the current export file layout is intentionally preserved
- blob handling is transparent and server-owned; there is no `blobs/` export directory and no need for a blob-list API
- best-effort consistency is sufficient because this command exists for debugging and inspection
- the server remains the only component allowed to access the underlying run store in this architecture
