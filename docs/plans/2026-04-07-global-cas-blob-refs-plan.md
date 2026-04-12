# Global CAS Blob Refs Plan

## Summary

Replace durable offload pointers with global content-addressed blob refs and keep file paths as an execution-only concern.

- Automatic large-value offload should persist `blob://sha256/<hex>` refs instead of `file://...` paths.
- Blob bytes should be stored in a global CAS namespace rather than under per-run keys.
- Handlers and preamble generation should continue to see file references, but those files should be materialized only in the execution-local environment.
- Host scratch blob cache files should stop being part of the durable contract.
- Garbage collection is explicitly deferred in this pass.

## Problem Frame

Large context values are currently offloaded by writing the serialized JSON bytes to the run store, materializing a host-side cache file, and replacing the original value with a `file://` pointer to that file.

That shape creates the wrong durable boundary:

- persisted checkpoints and checkpoint-completed events contain host- or sandbox-specific file paths instead of stable storage references
- resume seeds those path strings back into runtime state verbatim
- remote sandbox sync rewrites durable context into sandbox-local `file://` paths
- fork semantics are awkward because the same logical content becomes tied to a specific run and a specific materialized file path

The durable source of truth should be the blob bytes addressed by content hash. File paths should only exist as a temporary execution detail for agents and command handlers.

## Key Decisions

- Use `blob://sha256/<blob_id>` as the only new durable blob reference format.
  - `RunBlobId` remains the existing SHA-256 hex content hash type in this pass.
  - Durable run state, checkpoints, and checkpoint-completed event payloads should persist only plain JSON values and `blob://` refs.

- Make blob storage global CAS instead of run-scoped.
  - Internally, blob bytes move from per-run keys to global keys such as `blobs#sha256#<blob_id>`.
  - Existing run-scoped server endpoints can remain as the API surface for now, but they should read and write the global CAS store under the hood.

- Keep blob handling invisible to the model.
  - The model should continue to receive file references in prompts and preambles.
  - `blob://` is a backend durability protocol, not a model-facing protocol.

- Materialize blobs only in execution-local views.
  - Before handler execution and before preamble construction, resolve `blob://` refs into local files for the active sandbox.
  - For remote sandboxes, materialize under `{working_directory}/.fabro/blobs/<blob_id>.json`.
  - For local execution, materialize under a run-local ephemeral runtime directory such as `runtime/blobs/<blob_id>.json`.

- Do not persist execution-local `file://` refs back into durable state.
  - Managed materialized blob file refs must be normalized back to `blob://sha256/<blob_id>` before context snapshots are emitted or checkpointed.

- Preserve compatibility with older runs.
  - Read paths should continue to recognize legacy blob-backed `file://.../<blob_id>.json` values.
  - New writes should use only the `blob://` form.

- Leave stage artifacts unchanged.
  - This change applies only to offloaded run blobs.
  - `ArtifactStore` remains the durable system for captured stage artifacts.

- Defer GC.
  - New blob writes are append-only in this pass.
  - No mark-and-sweep, refcounting, or retention enforcement is included here.

## Implementation Changes

### 1. Blob Storage And Blob Ref Helpers

- Keep `RunBlobId` unchanged in `lib/crates/fabro-types/src/run_blob_id.rs`.
- Add a shared blob-ref helper module in `fabro-workflow` or `fabro-types` that:
  - formats `blob://sha256/<blob_id>`
  - parses `blob://sha256/<blob_id>`
  - recognizes legacy blob-backed `file://.../<blob_id>.json`
  - extracts blob ids from managed materialized blob file paths
- Change `fabro-store` blob key construction from `blobs#{run_id}#{blob_id}` to a global key layout such as `blobs#sha256#<blob_id>`.
- Update `RunDatabase::write_blob` and `RunDatabase::read_blob` to operate on the global CAS namespace.
- Remove blob enumeration from the main architecture path. `list_blobs` should not be part of new feature work; if retained temporarily, it should be treated as legacy/debug-only.

### 2. Automatic Offload

- In `lib/crates/fabro-workflow/src/artifact.rs`, change `offload_large_values` so that it:
  - serializes the JSON value
  - writes the bytes to CAS through the existing run-store handle
  - replaces the value with `blob://sha256/<blob_id>`
  - does not write a host-side cache file
- Remove the current assumption that `cache/artifacts/values/{blob_id}.json` is part of the durable contract.
- Keep the offload threshold unchanged at 100KB in this pass.

### 3. Execution-Time Materialization

- Replace `sync_artifacts_to_env` with an execution-time blob materialization flow that handles both:
  - new `blob://` refs
  - existing explicit or legacy `file://` refs
- Split this into two responsibilities:
  - blob resolution and materialization for managed blob refs
  - existing file-copy behavior for explicit `file://` refs that are not blob-backed
- Materialization behavior:
  - read the blob bytes from CAS
  - write the JSON bytes into a sandbox-usable file path
  - return a rewritten execution-local value using `file://<materialized_path>`
- Managed materialized paths should use a deterministic layout based on blob id so repeated refs dedupe naturally within an execution.

### 4. Context And Durable Snapshot Boundaries

- Introduce a clear split between:
  - durable context values
  - execution-local resolved context values
- Before handler execution, create a resolved execution view where `blob://` refs are rewritten to materialized `file://` refs.
- After handler execution, normalize any managed materialized blob file refs in handler-produced context changes back to `blob://sha256/<blob_id>`.
- Compatibility reads should normalize legacy blob-backed `file://.../<blob_id>.json` values to `blob://sha256/<blob_id>` in memory before they enter new durable snapshots.
- Update checkpoint creation and checkpoint-completed event emission so they snapshot only durable values, never execution-local materialized paths.
- Treat `current.preamble` as runtime-only derived state and exclude it from persisted context snapshots. This prevents preamble strings containing execution-local file paths from leaking into checkpoints or event payloads.

### 5. Preamble And Handler Execution

- Keep blobs invisible to the model.
- Before fidelity builds `current.preamble`, resolve completed-stage outcome values and current context into an execution-local view that contains file refs, not blob refs.
- `build_preamble` should continue to work with file references and should not mention `blob://` or “blobs” in user/model-facing output.
- Prompt and agent handlers should continue to consume `context.preamble()` and file references exactly as they do now.
- The only new behavior for handlers should be that the file refs they receive come from execution-time materialization rather than from durable checkpoint state.

### 6. Resume And Fork Behavior

- Resume should seed durable `blob://` values from checkpoint state and let the next execution hop materialize them as needed.
- Legacy checkpoints containing blob-backed `file://.../<blob_id>.json` values should be normalized on read so resumed runs persist the new `blob://` form on the next checkpoint.
- Fork should copy checkpoint and run state without copying blob payloads.
- Child runs should retain the same `blob://sha256/<blob_id>` refs as the source run.

### 7. CLI And Export Behavior

- Update CLI final-output rendering so when `response.*` is a blob ref it resolves the blob through the run-store read path before printing markdown.
- Keep explicit non-blob file refs as plain file references in CLI output.
- Refactor `store dump` to become reference-driven for blobs:
  - scan exported JSON structures for blob refs
  - fetch only referenced blobs
  - hydrate them inline in exported JSON
  - stop emitting a top-level `blobs/` directory
- Preserve the current export layout for run metadata, nodes, retro output, checkpoints, events, and stage artifacts.

### 8. Server And API Surface

- Keep the existing run-scoped blob routes:
  - `POST /api/v1/runs/{id}/blobs`
  - `GET /api/v1/runs/{id}/blobs/{blobId}`
- Change their implementation to use global CAS storage internally.
- Do not add public blob enumeration or global blob-fetch routes in this pass.
- Do not add GC or blob-membership verification to the API contract in this pass.

## Test Plan

### Blob Ref Helpers

- parse and format `blob://sha256/<blob_id>`
- recognize legacy blob-backed `file://.../<blob_id>.json`
- reject ordinary non-blob `file://` refs
- normalize managed materialized blob file refs back to blob refs

### Offload And Persistence

- large values are replaced with `blob://sha256/<blob_id>`
- offload writes the blob bytes to CAS
- offload no longer creates a host scratch cache file
- small values remain inline
- checkpoint and checkpoint-completed payloads persist `blob://` refs, not `file://`
- `current.preamble` is excluded from persisted context snapshots

### Execution Materialization

- local execution materializes `blob://` refs to local runtime files
- remote execution materializes `blob://` refs to sandbox files under `.fabro/blobs/`
- preamble generation receives file refs and does not expose `blob://`
- handlers receive file refs and can read them normally
- explicit non-blob `file://` refs keep their existing remote-copy behavior

### Normalization And Compatibility

- handler-produced managed materialized blob file refs are normalized back to `blob://` before checkpointing
- legacy blob-backed `file://.../<blob_id>.json` values are normalized to `blob://` on resume
- ordinary explicit `file://` refs are preserved as file refs
- resumed runs re-checkpoint using only the new `blob://` form

### Fork, CLI, And Export

- forked runs reuse the same `blob://sha256/<blob_id>` refs with no blob copy
- CLI final-output rendering resolves blob-backed final responses
- `store dump` hydrates referenced blobs inline
- `store dump` emits no top-level `blobs/` directory

## Important Files

- `lib/crates/fabro-workflow/src/artifact.rs`
- `lib/crates/fabro-workflow/src/lifecycle/artifact.rs`
- `lib/crates/fabro-workflow/src/lifecycle/fidelity.rs`
- `lib/crates/fabro-workflow/src/node_handler.rs`
- `lib/crates/fabro-workflow/src/handler/llm/preamble.rs`
- `lib/crates/fabro-workflow/src/pipeline/execute.rs`
- `lib/crates/fabro-workflow/src/records/checkpoint.rs`
- `lib/crates/fabro-workflow/src/lifecycle/event.rs`
- `lib/crates/fabro-store/src/keys.rs`
- `lib/crates/fabro-store/src/slate/run_store.rs`
- `lib/crates/fabro-server/src/server.rs`
- `lib/crates/fabro-cli/src/server_client.rs`
- `lib/crates/fabro-cli/src/commands/run/output.rs`
- `lib/crates/fabro-workflow/src/run_dump.rs`
- `docs/execution/context.mdx`
- `docs/agents/outputs.mdx`

## Assumptions And Defaults

- The durable blob ref format for this pass is `blob://sha256/<hex>`.
- `RunBlobId` remains the current type name even though blobs are no longer run-scoped.
- Global CAS uses the existing durable key-value store rather than introducing a new blob backend.
- Existing run-scoped blob HTTP routes remain the only supported transport surface in this pass.
- Blob lifecycle management and GC are explicitly deferred.
