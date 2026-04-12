# Run Manifest Blobs And Bundle Removal Plan

## Summary
Persist run-definition inputs in CAS-backed blob storage and stop treating scratch as a durable source of workflow definition state.

This pass stores two durable blob-backed payloads for `/runs` creates:

- the raw submitted `RunManifest` on `run.created`
- the smaller accepted internal run definition on `run.submitted`

It removes `workflow_bundle.json` from scratch and updates start/resume to load the accepted definition from the run store instead.

## Key Decisions
- Keep the current event sequence.
  - Do not add a separate `run.accepted` event.
  - `run.created` carries the raw submitted-manifest blob ref.
  - `run.submitted` carries the accepted-definition blob ref.
- Store the submitted manifest as the exact request-body bytes.
  - Motivation: preserve the exact wire submission for audit/debugging, not just the parsed semantic content.
  - Consequence: semantically equivalent JSON bodies with different whitespace or field ordering will produce different blob IDs.
- Treat this as greenfield work.
  - Do not add compatibility fallbacks for older event shapes, older runs, or older scratch layouts.
  - Do not preserve `workflow_bundle.json` reads or writes behind a migration shim.
- Keep `workflow_source` inline on `run.created`.
  - Cheap projection reads and existing graph-source consumers should continue to work without hydrating a blob.
- Rename `StoredWorkflowBundle` to `AcceptedRunDefinition` and make it the durable accepted-definition payload.
  - Keep the existing data shape (`workflow_path`, `workflows`) and add a `version` field.
  - Delete the file I/O helpers instead of introducing a second identical type with conversion boilerplate.
- Treat the accepted-definition pointer as part of run status state.
  - Because `run.submitted` is reused by rewind, every `run.submitted` emission must carry the accepted-definition blob ref from run state.

## Implementation Changes
### 1. Event and projection types
- Update [`lib/crates/fabro-types/src/run_event/run.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-types/src/run_event/run.rs):
  - add `submitted_manifest_blob: Option<RunBlobId>` to `RunCreatedProps`
  - replace `RunSubmitted(RunStatusTransitionProps)` with `RunSubmitted(RunSubmittedProps)`
  - define `RunSubmittedProps { reason: Option<StatusReason>, accepted_definition_blob: Option<RunBlobId> }`
- Update [`lib/crates/fabro-types/src/run_event/mod.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-types/src/run_event/mod.rs) and event-name plumbing to use the new `RunSubmittedProps`.
- Update [`lib/crates/fabro-types/src/run.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-types/src/run.rs) `RunRecord` with:
  - `submitted_manifest_blob: Option<RunBlobId>`
  - `accepted_definition_blob: Option<RunBlobId>`
- Update [`lib/crates/fabro-store/src/run_state.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-store/src/run_state.rs):
  - `run.created` seeds `submitted_manifest_blob`
  - `run.submitted` updates run status and overwrites `accepted_definition_blob`
  - `graph_source` continues to come from inline `workflow_source`

### 2. Durable run-definition blob payloads
- Update [`lib/crates/fabro-workflow/src/workflow_bundle.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-workflow/src/workflow_bundle.rs):
  - rename `StoredWorkflowBundle` to `AcceptedRunDefinition`
  - add a `version` field
  - remove `load_from_run_dir()` and any `workflow_bundle.json` file I/O helpers
- Keep `BundledWorkflow` and `WorkflowBundle` as runtime types used by execution and child-workflow resolution.
- Keep the current constructor/runtime helper surface where useful, but do not add a parallel accepted-definition type or a redundant conversion layer.

### 3. Server create path and blob persistence
- Update [`lib/crates/fabro-server/src/server.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-server/src/server.rs) `POST /runs`:
  - read the raw request body bytes
  - deserialize `RunManifest` from those bytes
  - pass both the typed manifest and the original bytes into workflow creation because this pass intentionally stores the exact submitted JSON bytes
- Update [`lib/crates/fabro-workflow/src/operations/create.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-workflow/src/operations/create.rs):
  - extend `CreateRunInput` with optional raw submitted-manifest bytes
  - after opening the run store, write the raw manifest bytes to CAS when present
  - derive the accepted definition from `workflow_path` + `workflow_bundle` and write it to CAS
  - remove the `persist_workflow_bundle()` call and delete the helper
  - append `run.created` with `submitted_manifest_blob`
  - append `run.submitted` with `accepted_definition_blob`
- All normal `/runs` creates, including CLI `fabro run` through the server route, should write both blobs.
- Direct low-level `CreateRunInput` callers that bypass manifests may omit `submitted_manifest_blob`.
  - If they still provide `workflow_path` + `workflow_bundle`, they should still get an `accepted_definition_blob`.
  - Only callers that provide neither manifest bytes nor a bundled workflow definition may leave both refs `None`.

### 4. Start, resume, and rewind
- Update [`lib/crates/fabro-workflow/src/operations/start.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-workflow/src/operations/start.rs):
  - stop reading `workflow_bundle.json` from `persisted.run_dir()`
  - load `state.run.accepted_definition_blob`
  - fetch the accepted-definition bytes through `RunStoreHandle`
  - deserialize the accepted definition and reconstruct `workflow_path` / `workflow_bundle`
- Leave `WorkflowInput::Path` behavior unchanged for truly non-bundled runs that never had an accepted-definition blob.
- Update [`lib/crates/fabro-cli/src/commands/run/rewind.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/src/commands/run/rewind.rs) so the re-emitted `run.submitted` event includes the current `accepted_definition_blob` from run state.
- Keep manager-loop and parallel child-workflow execution unchanged once `EngineServices.workflow_bundle` is hydrated from the accepted-definition blob.

### 5. Output and documentation cleanup
- Update CLI JSON/event rendering paths that special-case empty `run.submitted` properties so they tolerate structured `RunSubmittedProps`:
  - [`lib/crates/fabro-cli/src/commands/run/logs.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/src/commands/run/logs.rs)
  - [`lib/crates/fabro-cli/src/commands/run/attach.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/src/commands/run/attach.rs)
- Remove `workflow_bundle.json` from [`docs/reference/run-directory.mdx`](/Users/bhelmkamp/p/fabro-sh/fabro-2/docs/reference/run-directory.mdx).
- Update any tests or docs that mention `StoredWorkflowBundle` or scratch-based workflow bundle persistence.

## Public Interface Changes
- Internal event payload shape changes:
  - `run.created` gains `submitted_manifest_blob`
  - `run.submitted` now emits structured properties with `reason` and `accepted_definition_blob`
- Internal run-state shape changes:
  - `RunProjection.run` gains `submitted_manifest_blob` and `accepted_definition_blob`
- No public `/runs` request-shape change is intended.
- No new HTTP routes are required; existing run-scoped blob read/write APIs remain the storage surface.

## Test Plan
- Add create-path coverage in [`lib/crates/fabro-workflow/src/operations/create.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-workflow/src/operations/create.rs):
  - manifest-backed create writes both blobs
  - raw submitted-manifest bytes round-trip exactly through CAS
  - first event is `run.created` with `submitted_manifest_blob`
  - second event is `run.submitted` with `accepted_definition_blob`
  - no `workflow_bundle.json` file is written
- Add start/resume coverage in [`lib/crates/fabro-workflow/src/operations/start.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-workflow/src/operations/start.rs):
  - accepted-definition blob hydrates `workflow_bundle`
  - bundled imports/prompts/child workflows still resolve after original source files are removed
- Add projection/event coverage in [`lib/crates/fabro-store/src/run_state.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-store/src/run_state.rs) and event serde tests:
  - `run.created` stores `submitted_manifest_blob`
  - `run.submitted` updates `accepted_definition_blob`
  - serialized/deserialized `RunSubmittedProps` round-trips cleanly
- Add rewind coverage in [`lib/crates/fabro-cli/src/commands/run/rewind.rs`](/Users/bhelmkamp/p/fabro-sh/fabro-2/lib/crates/fabro-cli/src/commands/run/rewind.rs):
  - rewind re-emits `run.submitted` with the current accepted-definition blob
- Update CLI attach/log integration tests so `run.submitted` JSON includes structured properties and still renders correctly.

## Assumptions
- Greenfield strictness is preferred over compatibility padding.
- Orphaned manifest/definition blobs are acceptable in this pass; no blob GC work is included.
  - CAS deduplication limits duplicate growth because identical payloads share blob storage.
- The raw submitted-manifest blob is stored exactly as received over HTTP, not by reserializing a typed struct.
- The accepted definition is the only durable source used to reconstruct bundled workflow execution state after creation.
