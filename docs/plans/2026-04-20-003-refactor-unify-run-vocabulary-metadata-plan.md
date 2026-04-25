---
title: "refactor: unify run vocabulary and metadata snapshot layout"
type: refactor
status: active
date: 2026-04-20
origin: /Users/bhelmkamp/.claude/plans/make-a-full-plan-pure-wombat.md
deepened: 2026-04-20
---

# refactor: unify run vocabulary and metadata snapshot layout

## Overview

Align the run domain vocabulary and metadata-branch layout around the event-sourced projection the code already maintains in memory. The refactor renames `RunRecord` to `RunSpec`, collapses metadata snapshots into a trimmed `RunProjection` in `run.json`, normalizes per-stage files to `stages/{node_id}@{visit}/...`, and updates every metadata-branch consumer to read that unified shape.

## Problem Frame

The durable write path is already projection-oriented, but the git metadata branch still presents the same run through multiple accidental shapes:

- `run.json` is only the spec slice (`RunRecord`)
- run lifecycle state is split across `start.json`, `status.json`, `checkpoint.json`, `sandbox.json`, `retro.json`, and `conclusion.json`
- node payloads use multiple incompatible path conventions under `nodes/`
- fork, rewind, rebuild, retro upload, and CLI dump/export all read or write those legacy files directly

That mismatch leaks implementation history into the domain model and makes every consumer reason about special cases. The current tree on `main` still shows the old design in `RunDump`, `MetadataStore`, fork/rewind/rebuild operations, CLI rewind recovery, and `fabro store dump`. This plan makes the metadata branch a true serialized projection snapshot and removes the split-file vocabulary drift.

## Requirements Trace

- R1. Rename `RunRecord` to `RunSpec` and rename `RunProjection.run` to `RunProjection.spec` everywhere in Rust code and tests. Do not ship alias shims.
- R2. Introduce a metadata-only serializer that writes `run.json` as a trimmed `RunProjection`, stripping bulky `NodeState` text fields (`prompt`, `response`, `diff`, `stdout`, `stderr`) while preserving all other projection data.
- R3. Standardize metadata-branch and export layout around `run.json`, `graph.fabro`, `retro/*.md`, `events.jsonl`, `checkpoints/*.json`, artifact exports, and `stages/{node_id}@{visit}/...`. Stop writing top-level `start.json`, `status.json`, `checkpoint.json`, `sandbox.json`, `retro.json`, and `conclusion.json`.
- R4. Replace metadata helpers and writers that special-case `checkpoint.json` with snapshot-oriented helpers that can write a full projection commit and still return the metadata-branch commit SHA when checkpoint flows need it.
- R5. Update metadata consumers (`fork`, `rewind`, `rebuild_meta`, CLI rewind recovery, retro upload, store dump/export) to read the unified projection layout without changing user-visible behavior.
- R6. Add additive query methods on `RunSpec` and `RunProjection` for common reads while keeping existing public-field access valid.
- R7. Update crate tests, CLI integration tests, and snapshots to the new layout with no coverage regression.

## Scope Boundaries

- No backward-compatibility read path for old metadata branches. This repo is still pre-launch greenfield.
- No OpenAPI or generated TypeScript client change. Server APIs expose run state independently of metadata-branch file layout.
- Keep `graph.fabro`, `retro/prompt.md`, `retro/response.md`, `events.jsonl`, `checkpoints/*.json`, and artifact export support.
- Do not move artifact exports away from `artifacts/nodes/{node_id}/visit-{n}/...` unless implementation proves a hard blocker; artifact path cleanup is not the point of this refactor.
- Do not convert fork/rewind to read events directly from durable storage; they continue to operate from metadata branches.

## Context & Research

### Relevant Code and Patterns

- `lib/crates/fabro-types/src/run.rs` and `lib/crates/fabro-store/src/run_state.rs` define the core vocabulary and projection shape that this refactor renames and extends.
- `lib/crates/fabro-workflow/src/run_dump.rs` and `lib/crates/fabro-cli/src/commands/store/run_export.rs` currently duplicate layout/serialization logic and already drift on node path format.
- `lib/crates/fabro-workflow/src/lifecycle/git.rs` and `lib/crates/fabro-workflow/src/pipeline/finalize.rs` still use phase-specific `RunDump` constructors and a `checkpoint.json`-oriented metadata helper.
- `lib/crates/fabro-workflow/src/operations/{fork.rs,rewind.rs,rebuild_meta.rs}` plus `lib/crates/fabro-cli/src/commands/run/rewind.rs` are the critical metadata readers/writers that must switch from standalone `checkpoint.json` and `start.json` reads to projection reads.
- `lib/crates/fabro-types/src/stage_id.rs` already defines `Display` as `{node_id}@{visit}`, which should become the on-disk stage directory name.
- `docs-internal/testing-strategy.md` says CLI integration tests should remain command-driven and black-box; layout-specific assertions belong in the right layer rather than by planting run internals by hand.

### Institutional Learnings

- No matching `docs/solutions/` entries were present in this repo at planning time, so this plan is grounded in current code and test patterns rather than prior internal solution notes.

### External References

- None. This is an internal Rust refactor with sufficient local context.

## Key Technical Decisions

- Hard rename `RunRecord` to `RunSpec` and `RunProjection.run` to `RunProjection.spec`.
  Rationale: the current names are the main source of spec/projection confusion, and a greenfield codebase does not benefit from preserving legacy aliases.
- `run.json` becomes the single top-level serialized projection snapshot for metadata branches and exports, including `conclusion`.
  Rationale: leaving `conclusion.json` behind would preserve the accidental fragmentation this refactor is trying to remove.
- Use a dedicated metadata serializer wrapper instead of changing `RunProjection`'s canonical serde implementation.
  Rationale: ordinary projection serde remains valuable for tests and internal round-trips, while metadata snapshots need one specific trimmed representation.
- Normalize per-stage paths to `stages/{stage_id}/{filename}` using `StageId::Display`.
  Rationale: this removes visit-1 special cases and aligns the on-disk layout with the stage identifier already exposed in APIs and logs.
- Replace `MetadataStore::write_checkpoint` with a snapshot-oriented commit helper rather than passing renamed data through a stale `checkpoint_json` API.
  Rationale: checkpoint commits still need a returned SHA, but the helper should describe the new snapshot semantics instead of the deleted file.
- Delete the CLI-only `StoreRunExport` duplication and reuse the workflow dump builder.
  Rationale: this refactor changes layout semantics in one place; keeping two near-identical serializers would make future drift likely.
- Keep artifact exports under `artifacts/nodes/{node_id}/visit-{n}/...` in this unit.
  Rationale: artifact lookup is already keyed by `StageId` at API boundaries, but changing artifact paths would widen scope without addressing the metadata-vocabulary problem.
- Query methods remain additive.
  Rationale: field privacy is a follow-up concern, and this refactor already changes many call sites.

## Open Questions

### Resolved During Planning

- Should `conclusion.json` survive as a separate top-level file?
  No. It should collapse into `run.json` with the rest of the projection.
- Should CLI export keep its own serializer?
  No. Reuse the workflow dump/export builder so metadata branches and `fabro store dump` cannot diverge again.
- Does the layout change need to cover CLI rewind recovery as well as workflow operations?
  Yes. `lib/crates/fabro-cli/src/commands/run/rewind.rs` currently reads `checkpoint.json` from the metadata branch and must switch with the rest of the readers.

### Deferred to Implementation

- Exact helper names for the new metadata commit writer (`write_snapshot`, `write_projection_commit`, etc.). The plan fixes the API shape and intent, but the final Rust name can be chosen during implementation.
- Whether the shared export builder stays in `lib/crates/fabro-workflow/src/run_dump.rs` or moves to a nearby module. The key constraint is one authoritative layout builder, not a specific file name.
- Whether any low-value tests should move layers while being updated. Follow `docs-internal/testing-strategy.md` if implementation reveals a better layer, but do not turn this refactor into a broad test reorganization.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```text
Durable event store
    -> RunProjection { spec, start, status, checkpoint, conclusion, retro, sandbox, nodes, ... }
    -> Metadata serializer (trim bulky node text fields)
    -> Metadata/export tree:
       run.json                           # trimmed RunProjection snapshot
       graph.fabro                        # readable workflow source
       stages/<node@visit>/...            # prompt.md, response.md, status.json, provider_used.json,
                                          # diff.patch, script_invocation.json, script_timing.json,
                                          # parallel_results.json, stdout.log, stderr.log
       retro/prompt.md
       retro/response.md
       events.jsonl
       checkpoints/<seq>.json
       artifacts/nodes/<id>/visit-<n>/...
```

## Implementation Units

- [ ] **Unit 1: Rename run vocabulary to spec/projection**

**Goal:** Replace the legacy `RunRecord`/`run` vocabulary with `RunSpec`/`spec` across the domain model and its consumers.

**Requirements:** R1

**Dependencies:** None

**Files:**
- Modify: `lib/crates/fabro-types/src/run.rs`
- Modify: `lib/crates/fabro-types/src/lib.rs`
- Modify: `lib/crates/fabro-workflow/src/records/{run.rs,mod.rs}`
- Modify: `lib/crates/fabro-store/src/run_state.rs`
- Modify: `lib/crates/fabro-workflow/src/{runtime_store.rs,run_lookup.rs}`
- Modify: `lib/crates/fabro-workflow/src/pipeline/{pull_request.rs,retro.rs,types.rs,execute/tests.rs}`
- Modify: `lib/crates/fabro-workflow/src/operations/{create.rs,start.rs,fork.rs,rebuild_meta.rs}`
- Modify: `lib/crates/fabro-cli/src/commands/{run/create.rs,run/fork.rs,run/rewind.rs,runs/inspect.rs,pr/create.rs,store/dump.rs}`
- Modify: `lib/crates/fabro-server/src/server.rs`
- Test: `lib/crates/fabro-types/tests/run_record_serde.rs` (rename to `run_spec_serde.rs`)
- Test: `lib/crates/fabro-cli/tests/it/cmd/create.rs`

**Approach:**
- Make this a pure mechanical rename first so later layout changes can focus on behavior rather than symbol churn.
- Rename `Persisted::run_record` and other outward-facing internal helpers to `spec`-oriented names in the same pass.
- Keep the data shape unchanged in this unit; only names move.

**Execution note:** Land as a mechanical rename before touching metadata serialization or file layout.

**Patterns to follow:**
- `lib/crates/fabro-types/src/stage_id.rs` accessor style for the later query-method unit.

**Test scenarios:**
- Happy path: `run_spec_serde.rs` round-trips a `RunSpec` with templated settings and blob refs exactly as the old `RunRecord` test did.
- Happy path: `RunProjection::apply_event` stores `spec` on `RunCreated` and updates the spec's `definition_blob` on `RunSubmitted`.
- Edge case: workspace code compiles with no lingering `RunRecord` or `run_record` identifiers in Rust source.

**Verification:**
- The workspace compiles after the rename with no alias shims.
- Rust source no longer contains `RunRecord` or `run_record` identifiers.

- [ ] **Unit 2: Add trimmed projection serialization and additive query methods**

**Goal:** Define the metadata snapshot serialization contract and expose additive readers on `RunSpec` and `RunProjection`.

**Requirements:** R2, R6

**Dependencies:** Unit 1

**Files:**
- Create: `lib/crates/fabro-store/src/serializable_projection.rs`
- Modify: `lib/crates/fabro-store/src/lib.rs`
- Modify: `lib/crates/fabro-store/src/run_state.rs`
- Modify: `lib/crates/fabro-types/src/run.rs`
- Test: `lib/crates/fabro-store/src/serializable_projection.rs`
- Test: `lib/crates/fabro-store/src/run_state.rs`
- Test: `lib/crates/fabro-types/tests/run_spec_methods.rs`

**Approach:**
- Add a metadata-only serializer wrapper around `RunProjection` that strips `NodeState.prompt`, `response`, `diff`, `stdout`, and `stderr` from `run.json` while preserving top-level fields and the non-bulky node metadata.
- Keep ordinary `RunProjection` serde untouched so existing test helpers and internal round-trips keep working.
- Add query methods such as `RunSpec::id()`, `RunSpec::graph()`, `RunProjection::spec()`, `RunProjection::status()`, and `RunProjection::current_checkpoint()` without changing field visibility.

**Execution note:** Start with failing round-trip tests before wiring the new serializer into metadata writers.

**Patterns to follow:**
- Existing `RunProjection::node`, `iter_nodes`, and `list_node_visits` helpers in `lib/crates/fabro-store/src/run_state.rs`
- `StageId` accessor methods in `lib/crates/fabro-types/src/stage_id.rs`

**Test scenarios:**
- Happy path: a projection with full top-level state and one populated node round-trips through the metadata serializer, deserializes back, and keeps all non-bulky fields intact while clearing the bulky text fields.
- Happy path: `RunSpec` getters expose `run_id`, `graph`, `settings`, `workflow_slug`, `working_directory`, and labels from a representative fixture.
- Edge case: an empty projection round-trips unchanged.
- Edge case: projections containing `foo@1` and `foo@2` nodes preserve both `StageId` keys across the round-trip.
- Edge case: `RunProjection::status()` returns `None` when no status record exists and the correct enum when one does.

**Verification:**
- The metadata serializer can round-trip a projection into the trimmed wire shape and back.
- New accessors compile without forcing existing field access call sites to change.

- [ ] **Unit 3: Unify metadata and export writers around one snapshot layout**

**Goal:** Make one authoritative dump/export builder produce the unified `run.json` + `stages/` layout for both metadata branches and CLI export.

**Requirements:** R2, R3, R4

**Dependencies:** Unit 2

**Files:**
- Modify: `lib/crates/fabro-workflow/src/run_dump.rs`
- Modify: `lib/crates/fabro-workflow/src/lifecycle/git.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- Modify: `lib/crates/fabro-workflow/src/git.rs`
- Modify: `lib/crates/fabro-cli/src/commands/store/{dump.rs,run_export.rs}`
- Test: `lib/crates/fabro-workflow/src/git.rs`
- Test: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- Test: `lib/crates/fabro-cli/tests/it/cmd/store_dump.rs`

**Approach:**
- Replace `RunDump::metadata_init`, `metadata_checkpoint`, `metadata_finalize`, and the CLI-only `StoreRunExport::from_store_state_and_events` path with one authoritative builder that starts from a `RunProjection`.
- Have metadata snapshots always emit `run.json` through the trimmed serializer, `graph.fabro` when present, and stage files under `stages/{stage_id}/...`.
- Keep export-only concerns (`events.jsonl`, `checkpoints/*.json`, hydrated blobs, artifact bytes) as opt-in helpers on the shared builder rather than as a second serializer.
- Remove top-level split JSON files, including `conclusion.json`, from both metadata branches and CLI export.
- Update checkpoint persistence in lifecycle code to use the new generic snapshot commit helper instead of a `checkpoint.json`-specific API.

**Patterns to follow:**
- Existing `RunDumpEntry` helpers in `lib/crates/fabro-workflow/src/run_dump.rs`
- `StageId::Display` in `lib/crates/fabro-types/src/stage_id.rs`

**Test scenarios:**
- Happy path: an init-state projection writes only `run.json` and `graph.fabro` when no stages or retro data exist.
- Happy path: a checkpoint-state projection writes `run.json` plus `stages/<id>@1/` files for prompt, response, status, provider, diff, script metadata, stdout, and stderr when present.
- Happy path: CLI dump/export uses the same builder and still emits `events.jsonl`, `checkpoints/*.json`, `retro/*.md`, and artifact payloads.
- Edge case: a node with multiple visits writes both `stages/build@1/...` and `stages/build@2/...` with no visit-1 special case.
- Edge case: `run.json` contains `start`, `status`, `checkpoint`, `sandbox`, `retro`, and `conclusion`, but not bulky node text payloads.

**Verification:**
- Writer/export code no longer contains legacy `nodes/` metadata stage paths or top-level split-file emission logic.
- Shared writer tests prove metadata branches and CLI export emit the same projection layout.

- [ ] **Unit 4: Update metadata readers, recovery flows, and rebuild logic**

**Goal:** Move every metadata-branch consumer from standalone file reads to projection reads, including the rebuild and rewind recovery paths.

**Requirements:** R4, R5

**Dependencies:** Unit 3

**Files:**
- Modify: `lib/crates/fabro-checkpoint/src/metadata.rs`
- Modify: `lib/crates/fabro-workflow/src/operations/{fork.rs,rewind.rs,rebuild_meta.rs}`
- Modify: `lib/crates/fabro-cli/src/commands/run/rewind.rs`
- Test: `lib/crates/fabro-checkpoint/src/metadata.rs`
- Test: `lib/crates/fabro-workflow/src/operations/{fork.rs,rewind.rs,rebuild_meta.rs}`
- Test: `lib/crates/fabro-cli/tests/it/cmd/fork.rs`
- Test: `lib/crates/fabro-cli/tests/it/scenario/recovery.rs`
- Test: `lib/crates/fabro-workflow/tests/it/{integration.rs,daytona_integration.rs}`

**Approach:**
- Add `MetadataStore::read_run_projection` and `read_run_spec`; either delete `read_checkpoint`/`read_start_record` or demote them to projection-field extractors after callers switch.
- Update `fork` to read the source projection, clone the spec/start/sandbox slices it intentionally carries forward, inject the new run ID, and write the new run's metadata branch through the unified snapshot writer.
- Update rewind parallel detection to read `projection.spec.graph`, and update CLI rewind recovery to pull the restored checkpoint from the projection snapshot instead of `checkpoint.json`.
- Rewrite `rebuild_meta` to emit one snapshot commit per metadata commit (init/checkpoint/finalize) through the shared writer while preserving `git_commit_sha` backfill semantics inside `projection.checkpoint`.

**Patterns to follow:**
- Existing timeline and run-SHA backfill helpers in `lib/crates/fabro-workflow/src/operations/{rewind.rs,rebuild_meta.rs}`
- `RunStoreHandle::state()` projection access in `lib/crates/fabro-workflow/src/runtime_store.rs`

**Test scenarios:**
- Happy path: a forked run gets a new `run.json` projection with the new run ID, inherited sandbox/start context, and no top-level split JSON files.
- Error path: forking still fails cleanly when the source metadata branch lacks `run.json` or the target checkpoint lacks a run commit SHA.
- Happy path: rewind parallel detection still recognizes interior parallel groups from `projection.spec.graph`.
- Happy path: CLI rewind recovery reads the checkpoint from the projection snapshot and replays `RunRewound` plus restored checkpoint events correctly.
- Integration: rebuild-meta emits one `run.json` snapshot per metadata commit, and each snapshot contains the expected checkpoint payload and backfilled `git_commit_sha`.
- Error path: rebuild-meta remains atomic on failure and still refuses to overwrite an existing metadata branch.

**Verification:**
- Metadata consumers no longer require top-level `checkpoint.json`, `start.json`, or `sandbox.json`.
- Fork, rewind, and rebuild tests pass against the unified layout.

- [ ] **Unit 5: Sweep downstream docs, retro prompts, and snapshots**

**Goal:** Align retro tooling, integration tests, and snapshots with the unified metadata vocabulary and file layout.

**Requirements:** R3, R5, R7

**Dependencies:** Unit 4

**Files:**
- Modify: `lib/crates/fabro-retro/src/retro_agent.rs`
- Modify: `lib/crates/fabro-cli/tests/it/cmd/{store_dump.rs,start.rs,fork.rs}`
- Modify: `lib/crates/fabro-cli/tests/it/scenario/recovery.rs`
- Modify: `lib/crates/fabro-workflow/src/git.rs`
- Modify: `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- Modify: `lib/crates/fabro-workflow/tests/it/{integration.rs,daytona_integration.rs}`
- Test: the files above

**Approach:**
- Update retro agent instructions and sandbox uploads so the agent reads `run.json` projection data plus `graph.fabro` and stage files instead of `checkpoint.json` and `start.json`.
- Rename or replace tests that currently assert `conclusion.json` or old `nodes/...` layouts so they assert conclusion presence inside `run.json` and stage files under `stages/`.
- Keep CLI integration tests black-box per `docs-internal/testing-strategy.md`; layout assertions should come from public command behavior or crate-level tests, not hand-planted run internals.
- Review snapshot diffs before accepting them because this refactor intentionally changes many file paths and exported filenames.

**Patterns to follow:**
- Snapshot discipline in `docs-internal/testing-strategy.md`
- Existing retro upload flow in `lib/crates/fabro-retro/src/retro_agent.rs`

**Test scenarios:**
- Happy path: retro sandbox upload includes `run.json` projection data, and the prompt tells the retro agent to inspect `run.json` plus `graph.fabro`/stage files rather than `checkpoint.json`.
- Happy path: `fabro store dump` snapshots show `run.json`, `graph.fabro`, `stages/...`, `retro/*.md`, `events.jsonl`, and `checkpoints/*.json`, with no legacy split JSON files.
- Happy path: integration and Daytona tests read run spec and checkpoint data through the new projection helpers and still observe correct `git_commit_sha` behavior.
- Edge case: tests that previously referred to missing `status.json` or `sandbox.json` continue to assert the public command behavior without relying on those internal filenames existing.

**Verification:**
- Snapshot and integration tests reference only the new layout.
- Retro tooling and test names no longer describe deleted files such as `conclusion.json` or `checkpoint.json` as metadata-branch invariants.

## System-Wide Impact

- **Interaction graph:** metadata snapshots are written from lifecycle init/checkpoint/finalize and rebuild-meta; they are read by fork, rewind, CLI rewind recovery, retro upload, store dump/export, and metadata-focused tests.
- **Error propagation:** deserialization errors shift from file-specific entities (`checkpoint`, `run record`) to projection parsing plus field-extraction errors; reader helpers should preserve branch/path context so failures stay diagnosable.
- **State lifecycle risks:** partial migration of writers/readers would silently break metadata-driven flows; the refactor must switch readers and writers in the same series and preserve checkpoint commit SHA capture.
- **API surface parity:** `StageId` already uses `node@visit`, so metadata paths, CLI exports, and test fixtures should align on the same identifier format. Artifact exports are the deliberate exception in this unit and remain on `artifacts/nodes/...`.
- **Integration coverage:** the highest-value end-to-end paths are checkpoint persistence, fork from checkpoint, rewind + resume recovery, rebuild metadata from durable state, and `fabro store dump`.
- **Unchanged invariants:** event semantics, durable-store state accumulation, `graph.fabro` export, and artifact export support remain intact; the refactor changes metadata serialization shape, not workflow execution behavior.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| A reader still depends on `checkpoint.json`, `start.json`, or `sandbox.json` after those files stop being written | Exhaustively update metadata helper call sites and keep dedicated fork/rewind/recovery integration coverage in the same series |
| `run.json` trimming accidentally drops state consumers still need | Add round-trip tests that prove all non-bulky top-level and node metadata survives the trimmed serializer |
| Shared writer migration leaves CLI export and metadata branches on subtly different layouts | Delete or subsume `StoreRunExport` in the same series rather than maintaining parallel serializers |
| `git_commit_sha` handling regresses during fork or rebuild | Preserve dedicated tests for missing-SHA errors, backfilled SHAs, and forked checkpoint snapshots |
| Large mechanical rename obscures behavioral regressions in review | Land the rename first, keep later units behavior-focused, and use targeted tests for each behavior-bearing unit |

## Documentation / Operational Notes

- Update inline comments, test names, and docstrings that still describe `run.json` as a run record or refer to `checkpoint.json`, `status.json`, `sandbox.json`, or `nodes/...` as metadata-branch invariants.
- No rollout or migration plan is needed for existing branches because the repo is still pre-launch; local stale metadata branches can be regenerated or discarded.
- Snapshot updates should follow the repo's `cargo insta pending-snapshots` discipline rather than bulk-accepting blindly.

## Sources & References

- **Source plan:** `/Users/bhelmkamp/.claude/plans/make-a-full-plan-pure-wombat.md`
- Related code:
  - `lib/crates/fabro-types/src/run.rs`
  - `lib/crates/fabro-store/src/run_state.rs`
  - `lib/crates/fabro-workflow/src/run_dump.rs`
  - `lib/crates/fabro-checkpoint/src/metadata.rs`
  - `lib/crates/fabro-workflow/src/operations/{fork.rs,rewind.rs,rebuild_meta.rs}`
  - `lib/crates/fabro-cli/src/commands/{store/dump.rs,store/run_export.rs,run/rewind.rs}`
- Related guidance: `docs-internal/testing-strategy.md`
