# Metadata Snapshot Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Add first-class Fabro run events for durable metadata snapshot writes so run timing gaps are visible without emitting low-level git span events.

**Architecture:** Metadata snapshot events are product-domain workflow events emitted around each real metadata archive attempt. Callers own event emission around the whole metadata operation, including run-store state loading; `SandboxMetadataWriter` remains responsible for creating and pushing snapshots and returns snapshot accounting data.

**Tech Stack:** Rust, serde, strum, Fabro typed `Event`/`RunEvent` pipeline, existing CLI log/progress renderers, `cargo nextest`.

---

## Summary

Add these event names:

- `metadata.snapshot.started`
- `metadata.snapshot.completed`
- `metadata.snapshot.failed`

The events cover Fabro metadata snapshots only, not every underlying git or filesystem operation. Emit them for `init`, `checkpoint`, and `finalize` metadata attempts so timing gaps become visible without rebuilding tracing as events.

## Event Contract

- `MetadataSnapshotStartedProps { phase: MetadataSnapshotPhase, branch: String }`
- `MetadataSnapshotCompletedProps { phase: MetadataSnapshotPhase, branch: String, duration_ms: u64, entry_count: usize, bytes: u64, commit_sha: String }`
- `MetadataSnapshotFailedProps { phase: MetadataSnapshotPhase, branch: String, duration_ms: u64, failure_kind: MetadataSnapshotFailureKind, error: String, causes: Vec<String>, commit_sha: Option<String>, entry_count: Option<usize>, bytes: Option<u64> }`

Enums:

- `MetadataSnapshotPhase = init | checkpoint | finalize`
- `MetadataSnapshotFailureKind = load_state | write | push`
- Both enums must pair `#[serde(rename_all = "snake_case")]` with `#[strum(serialize_all = "snake_case")]` so serde and strum stay aligned with the project enum convention.

Rules:

- `metadata.snapshot.completed` means the metadata snapshot was committed and pushed successfully. There is no `pushed` field because it would always be true.
- `commit_sha` is intentionally asymmetric: completed snapshots always include `commit_sha: String`; failed snapshots include `commit_sha: Option<String>` because push failures can have a local commit while load-state and write failures cannot.
- Failed accounting fields are optional: `entry_count: Option<usize>` and `bytes: Option<u64>` are `Some` for push failures and `None` for load-state/write failures.
- Optional fields use `#[serde(default, skip_serializing_if = "Option::is_none")]`, matching the convention in `infra.rs`.
- Failed props follow the existing failure-event convention in `infra.rs`: `error: String` contains the primary error summary and `causes: Vec<String>` contains the cause chain with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- A writer `push_error` maps to `metadata.snapshot.failed { failure_kind: "push", commit_sha: Some(...), entry_count: Some(...), bytes: Some(...) }`.
- A run-store `state()` failure maps to `metadata.snapshot.failed { failure_kind: "load_state", commit_sha: None, entry_count: None, bytes: None }`.
- If metadata is already degraded and a later snapshot would currently return early, emit no metadata snapshot event for that skipped attempt. Do not emit `started`; skipped attempts are not real attempts and should not count as failures.
- Writer errors before a local commit map to `metadata.snapshot.failed { failure_kind: "write", commit_sha: None, entry_count: None, bytes: None }`.
- Emit `metadata.snapshot.failed` before the compatibility `run.notice` for the same failure so human-facing consumers can deterministically suppress duplicate warning text.
- Typed `metadata.snapshot.*` events are not deduplicated for real attempts. Existing metadata `run.notice` deduping remains compatibility-only.
- Checkpoint metadata events use the existing stage scope so they include `node_id`, `node_label`, and `stage_id`. Init/finalize metadata events are unscoped and must not set `node_id`, `node_label`, or `stage_id`.
- Keep `branch` because the exact metadata ref is useful in raw logs and for push failure diagnostics. Do not include `message`; `phase` fully identifies the logical metadata operation.

## Implementation Tasks

### Task 1: Add Typed Event Bodies

**Files:**
- Modify: `lib/crates/fabro-types/src/run_event/mod.rs`
- Modify: `lib/crates/fabro-types/src/run_event/infra.rs`

- [x] Add `MetadataSnapshotPhase` and `MetadataSnapshotFailureKind` enums in `infra.rs`.
- [x] Derive `Serialize`, `Deserialize`, `strum::Display`, `strum::EnumString`, and `strum::IntoStaticStr`.
- [x] Add both `#[serde(rename_all = "snake_case")]` and `#[strum(serialize_all = "snake_case")]` to each enum.
- [x] Add the three metadata snapshot props structs in `infra.rs` with the exact fields from the Event Contract section.
- [x] Add serde attributes for optional failed fields and empty `causes` exactly as specified in the Event Contract.
- [x] Add three `EventBody` variants in `mod.rs` with exact serde names:
  - `metadata.snapshot.started`
  - `metadata.snapshot.completed`
  - `metadata.snapshot.failed`
- [x] Extend `EventBody::event_name()` and known-event handling for all three names.

### Task 2: Add Workflow Event Variants And Mapping

**Files:**
- Modify: `lib/crates/fabro-workflow/src/event.rs`

- [x] Add internal `Event` variants matching the three new event bodies.
- [x] Extend `Event::trace()` with concise tracing fields: `phase`, `branch`, `duration_ms`, and `failure_kind`.
- [x] Extend `event_name()` with the three exact event names.
- [x] Extend `event_body_from_event()` to construct the matching `EventBody` variants.
- [x] Ensure unscoped metadata snapshot events do not set envelope `node_id`, `node_label`, or `stage_id`; checkpoint callers will use `emit_scoped()`.

### Task 3: Return Snapshot Accounting From The Writer

**Files:**
- Modify: `lib/crates/fabro-workflow/src/sandbox_metadata.rs`

- [x] Extend `MetadataSnapshot` to include `entry_count: usize` and `bytes: u64`.
- [x] Compute `entry_count` and `bytes` inside `SandboxMetadataWriter::write_snapshot()` from the single `dump.git_entries()` allocation that the writer already needs.
- [x] Return those accounting values on successful local metadata commit, including the case where `push_error` is present.
- [x] Do not add per-command events or expose writer-internal steps on the wire.

### Task 4a: Emit Init Metadata Events

**Files:**
- Modify: `lib/crates/fabro-workflow/src/lifecycle/git.rs`

- [x] If metadata is already degraded, return from the init metadata path without emitting `metadata.snapshot.*`.
- [x] Move the degraded check above the init `metadata.snapshot.started` emission point; the existing check inside `write_metadata_snapshot()` is not enough because skipped attempts must not leave a dangling `started`.
- [x] Keep the existing inner `metadata_degraded()` guard in `GitLifecycle::write_metadata_snapshot()` as defense-in-depth for future callers, but do not rely on it for init/checkpoint skip semantics.
- [x] Wrap the full init metadata operation in `GitLifecycle::on_run_start`, including `run_store.state()`.
- [x] Emit `metadata.snapshot.started { phase: "init" }` before loading run state for the init operation.
- [x] Emit `metadata.snapshot.completed` only when the init metadata commit and push both succeed.
- [x] Emit `metadata.snapshot.failed` for init load-state, write, and push failures using the Event Contract mapping.
- [x] Emit `metadata.snapshot.failed` before calling `emit_metadata_warning()` for the same init failure.

### Task 4b: Emit Checkpoint Metadata Events

**Files:**
- Modify: `lib/crates/fabro-workflow/src/lifecycle/git.rs`

- [x] If metadata is already degraded, return from the checkpoint metadata path without emitting `metadata.snapshot.*`.
- [x] Move the degraded check above the checkpoint `metadata.snapshot.started` emission point; the existing check inside `write_metadata_snapshot()` is not enough because skipped attempts must not leave a dangling `started`.
- [x] Keep the existing inner `metadata_degraded()` guard in `GitLifecycle::write_metadata_snapshot()` as defense-in-depth for future callers, but do not rely on it for init/checkpoint skip semantics.
- [x] Wrap the full checkpoint metadata operation in `GitLifecycle::on_checkpoint`, including `run_store.state()`.
- [x] Emit scoped `metadata.snapshot.started { phase: "checkpoint" }` before loading run state for the checkpoint operation.
- [x] Emit scoped `metadata.snapshot.completed` or `metadata.snapshot.failed` before `checkpoint.completed`.
- [x] Emit scoped `metadata.snapshot.failed` before calling `emit_metadata_warning()` for the same checkpoint failure.
- [x] Preserve existing metadata-degraded `run.notice` emission for compatibility, but treat the new typed event as the primary human-facing signal.

### Task 5: Emit Finalize Metadata Events

**Files:**
- Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`

- [x] If metadata is already degraded, return from the final metadata path without emitting `metadata.snapshot.*`.
- [x] Keep the degraded check above the final `metadata.snapshot.started` emission point; skipped attempts must not leave a dangling `started`.
- [x] Wrap the full final metadata operation in `write_finalize_commit`, including `run_store.state()`.
- [x] Emit `metadata.snapshot.started { phase: "finalize" }` before loading run state for the final metadata operation.
- [x] Emit `metadata.snapshot.completed` only when the final metadata commit and push both succeed.
- [x] Emit `metadata.snapshot.failed` for finalize load-state, write, and push failures using the Event Contract mapping.
- [x] Emit `metadata.snapshot.failed` before calling `emit_metadata_warning()` for the same final metadata failure.
- [x] Ensure final metadata events are emitted before `run.completed`.
- [x] Preserve existing `checkpoint_metadata_write_failed`, `checkpoint_metadata_push_failed`, and `checkpoint_metadata_degraded` notices for compatibility.

### Task 6: CLI, Consumers, And Documentation

**Files:**
- Modify: `lib/crates/fabro-cli/src/commands/run/logs.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/run_progress/event.rs`
- Modify: `lib/crates/fabro-cli/src/commands/run/run_progress/mod.rs`
- Modify: `docs/internal/events.md`

- [x] Audit existing consumers with `rg -n "event_name|EventBody|metadata.snapshot" lib/crates apps lib/packages docs/public/api-reference/fabro-api.yaml docs/internal --glob '!docs/superpowers/**'` and update any event-name filters that should recognize metadata snapshot events. Ignore matches in implementation-plan docs.
- [x] Render completed metadata snapshots compactly in pretty/progress output, for example `Metadata checkpoint 2.8s`.
- [x] Render `metadata.snapshot.failed` as the primary user-visible metadata warning/error.
- [x] Suppress duplicate CLI display of the compatibility `checkpoint_metadata_*` notice when the same stream already contains a matching earlier `metadata.snapshot.failed` event.
- [x] Limit suppression to per-failure compatibility notices: `checkpoint_metadata_write_failed` and `checkpoint_metadata_push_failed`. Do not suppress the `checkpoint_metadata_degraded` end-of-run summary notice; it is a distinct summary signal.
- [x] Keep `fabro logs --json` unchanged except for the new serialized event records.
- [x] Document the three event definitions in `docs/internal/events.md`.
- [x] State in the docs that these are product events for durable metadata snapshots, not tracing spans.

## API And Client Compatibility

Checked current API/client shape:

- `docs/public/api-reference/fabro-api.yaml` models `RunEvent` as a generic object with `event: string` and `properties: object`.
- `lib/packages/fabro-api-client/src/models/run-event.ts` includes `[key: string]: any` and `properties?: { [key: string]: any }`.

No OpenAPI or TypeScript client schema changes are required for this event-only addition unless implementation discovers a stricter consumer outside this model.

## Test Plan

- [x] Add `fabro-types` serialization/deserialization tests proving the three event names are known and props serialize to the agreed JSON shape.
- [x] Add `fabro-workflow` event conversion tests for all three variants, including scoped checkpoint metadata events.
- [x] Add lifecycle tests covering successful metadata snapshot emission: started then completed.
- [x] Add lifecycle tests covering `push_error` mapping to `metadata.snapshot.failed { failure_kind: "push", commit_sha: Some(...), entry_count: Some(...), bytes: Some(...) }`.
- [x] Add lifecycle tests covering degraded short-circuit behavior: no `metadata.snapshot.*` event is emitted and no `metadata.snapshot.started` event is left unterminated.
- [x] Add a cross-phase degraded test: after an init `metadata.snapshot.failed` marks metadata degraded, subsequent checkpoint and finalize attempts emit no `metadata.snapshot.*` events.
- [x] Add lifecycle tests covering pre-writer `run_store.state()` failure emission for init: started then failed with `failure_kind: "load_state"`.
- [x] Add lifecycle tests covering pre-writer `run_store.state()` failure emission for checkpoint: started then failed with `failure_kind: "load_state"`.
- [x] Add lifecycle tests covering pre-writer `run_store.state()` failure emission for finalize: started then failed with `failure_kind: "load_state"`.
- [x] Add tests proving `completed.entry_count` and `completed.bytes` equal the writer's `MetadataSnapshot` values.
- [x] Add tests proving push-failure `failed.entry_count` and `failed.bytes` equal the writer's `MetadataSnapshot` values.
- [x] Add ordering tests proving checkpoint metadata events occur before `checkpoint.completed`.
- [x] Add ordering tests proving finalize metadata events occur before `run.completed`.
- [x] Add tests proving `metadata.snapshot.failed` is emitted before the matching compatibility `run.notice`.
- [x] Add tests proving compatibility `run.notice` still fires for metadata degradation while CLI display avoids duplicate warnings.
- [x] Add CLI rendering tests for pretty/progress output so metadata events display compactly and do not break generic log output.
- [x] Run:

```bash
cargo nextest run -p fabro-types
cargo nextest run -p fabro-workflow metadata
cargo nextest run -p fabro-cli logs
cargo +nightly-2026-04-14 fmt --check --all
```

## Assumptions

- The public wire shape uses the exact event names in this plan.
- `entry_count` is the number of metadata files in the snapshot.
- `bytes` is the sum of serialized metadata entry byte lengths.
- Existing metadata-degraded notices stay for compatibility, but typed metadata snapshot events become the preferred signal for humans and new consumers.
- Already-degraded skipped attempts are intentionally silent; the first real failure event and compatibility notice explain why later metadata work is skipped.
- Runs with no configured metadata branch stay silent for metadata snapshot events because metadata snapshots are out of scope for those runs.
- A panic between `metadata.snapshot.started` and `metadata.snapshot.completed`/`metadata.snapshot.failed` may leave a dangling started event; this feature treats that as a run-level crash case rather than adding panic recovery around metadata event emission.
- The implementation should not introduce metadata writer sub-step events or expose low-level git command boundaries on the event stream.
