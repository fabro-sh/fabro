# Plan: Events as Source of Truth Follow-Ups

Close the remaining event-contract gaps required before we can execute `~/.claude/plans/memoized-pondering-knuth.md` and make projected `RunState` the primary read model.

## Context

`docs-internal/plan-events-as-source-of-truth.md` has mostly landed. The event stream is materially stronger now, but it still does not cover every field that `memoized-pondering-knuth.md` wants to derive from events.

The next step is not a broad store refactor. It is a narrow follow-up pass that:

- finishes the missing event coverage
- makes the remaining source-of-truth boundaries explicit
- leaves `memoized-pondering-knuth.md` with no hidden event-contract assumptions

This plan is a prerequisite plan, not the full event-sourced store migration.

## Simplification Rules

These rules govern every follow-up event change in this document:

- enrich an existing semantic event before inventing a new one
- keep run-level summary data on run-level events
- keep handler-specific metadata on handler-specific events
- avoid storage-shaped event names like `*.recorded`, `*.persisted`, or `*.written`
- explicitly retain non-event concerns instead of half-eventizing them

If a proposed event change violates one of those rules, prefer a simpler shape.

## Goal

After this plan lands, the later `memoized-pondering-knuth.md` work should be able to:

- build a projected `RunState` without inventing missing data
- replace direct read-side APIs with projection helpers
- remove duplicated write-side persistence for all event-backed fields

without first having to stop and redesign the event model again.

## Relationship To The Existing Plans

### What `plan-events-as-source-of-truth.md` already solved

- `run.created`
- `stage.completed.response`
- `sandbox.initialized` sandbox metadata
- `checkpoint.completed.diff`
- `command.started` / `command.completed`
- `retro.started.prompt/provider/model`
- `retro.completed.response/retro`

### What is still missing for `memoized-pondering-knuth.md`

- semantic run status events
- checkpoint events that can reconstruct full checkpoint snapshots and history
- full pull request record coverage
- final patch coverage
- provider-used coverage
- parallel-results coverage
- an explicit decision for fields that should remain non-event for now

## Decisions

### 1. Use semantic run lifecycle events, not a generic `run.status_changed`

Status should be reconstructed from explicit run lifecycle events rather than a generic “status changed” envelope.

Add event coverage for:

- `run.submitted`
- `run.starting`
- `run.running`
- `run.paused`
- `run.removing`
- `run.completed`
- `run.failed`
- `run.dead`

Projection rule:

- the latest status-bearing run event defines `RunStatusRecord.status`
- event-specific fields define `RunStatusRecord.reason`
- envelope `ts` defines `RunStatusRecord.updated_at`

`run.started` remains the start-record / execution-metadata event, not the canonical status event.

This separation is intentional:

- `run.started` answers "when and how did execution begin?"
- `run.running` answers "what is the run's status?"

Status mapping table:

| Event | Projected `RunStatus` | `StatusReason` rule |
|---|---|---|
| `run.submitted` | `Submitted` | `None` |
| `run.starting` | `Starting` | optional if the emitter has a concrete reason, otherwise `None` |
| `run.running` | `Running` | `None` |
| `run.paused` | `Paused` | preserve emitted reason if present |
| `run.removing` | `Removing` | `None` |
| `run.completed` | `Succeeded` | preserve emitted reason if present; default should remain `Completed` or `PartialSuccess` based on terminal outcome |
| `run.failed` | `Failed` | preserve emitted reason if present; expected common reasons include workflow/bootstrap/sandbox failures |
| `run.dead` | `Dead` | preserve emitted reason if present; otherwise `None` |

If any current status mutation cannot be represented cleanly by this table, fix the event model in this follow-up plan rather than pushing ambiguity into the later projector.

### 2. Keep the boundary tight: not every stored value must become an event in this pass

This follow-up plan should only eventize the fields that block the later projected-state cutover.

Retain as non-event concerns for now:

- binary assets
- artifact value blobs / offloaded context artifacts

This means `memoized-pondering-knuth.md` should be updated afterwards so `artifact_values` is no longer listed as a required event-backed row for the first cut.

### 3. Prefer complete event payloads over event joins that require hidden store lookups

If a projected record needs fields that do not already exist in another authoritative event, add them directly to the relevant event.

Do not rely on:

- sidecar JSON files
- legacy store records
- “the caller can join this with some other direct read”

Prefer one self-contained semantic event over reconstructing a record from several unrelated low-level events when that reconstruction adds complexity for little value.

## Follow-Up Coverage Matrix

This matrix is the contract for this plan. Each row must be green before `memoized-pondering-knuth.md` starts removing read/write APIs.

| Field / record needed later | Current direct source | Current event state | Follow-up required |
|---|---|---|---|
| `RunStatusRecord` | `put_status` in create/start/resume/finalize/disk paths | Incomplete; no semantic status event family | Add semantic run lifecycle events and a status mapping table |
| `StartRecord` | `put_start` | Mostly covered by `run.started` | Verify `run.started` fully covers `run_branch`, `base_sha`, `start_time`; no shape change if already true |
| latest `Checkpoint` | `put_checkpoint` | Incomplete; `checkpoint.completed` only carries `node_id`, `status`, `git_commit_sha`, `diff` | Enrich `checkpoint.completed` to carry a full checkpoint snapshot payload |
| checkpoint history | `append_checkpoint` / `list_checkpoints` | Incomplete; history cannot be rebuilt from current event payload | Use fully-populated `checkpoint.completed` as append-only checkpoint history |
| `Conclusion` | `put_conclusion` | Partially covered by terminal events plus stage aggregation | Verify the projector can derive full `Conclusion`, including retries/tokens/stage summaries, from existing events; if projection stays awkward, enrich terminal run events instead of adding new conclusion-only events |
| `PullRequestRecord` | `put_pull_request` | Incomplete; `pull_request.created` only carries URL/number/draft | Enrich `pull_request.created` to carry full `PullRequestRecord` fields |
| final patch | `put_final_patch` | Incomplete; terminal run event carries final SHA but not patch text | Enrich `run.completed` with `final_patch` |
| node provider metadata | `put_node_provider_used` | Incomplete; prompt/CLI events carry provider/model, but agent-mode still relies on sidecar sync | Project from existing handler-specific events and enrich forwarded agent session events if needed |
| node parallel results | `put_node_parallel_results` | Incomplete; `parallel.branch.completed.head_sha` is not enough | Enrich `parallel.completed` to carry the final results payload |
| node diff | `put_node_diff` | Partially covered by `checkpoint.completed.diff` | Decide and document whether node diff is sourced from the latest checkpoint event for that node or a dedicated node diff event; keep one canonical rule |
| retro prompt/response/retro payload | `put_retro_prompt`, `put_retro_response`, `put_retro` | Covered | No new event work; just parity-test it |
| sandbox record | `put_sandbox` | Covered | No new event work; just parity-test it |

## Required Event Changes

### 1. Add semantic run lifecycle events

Add new `Event` variants for:

- `RunSubmitted`
- `RunStarting`
- `RunRunning`
- `RunPaused`
- `RunRemoving`
- `RunDead`

Existing terminal events remain:

- `run.completed`
- `run.failed`

Required payload fields:

- `reason: Option<StatusReason>` where applicable
- any extra fields already emitted on terminal events should stay there

Emit from the same places that currently call `put_status`:

- `lib/crates/fabro-workflow/src/operations/create.rs`
- `lib/crates/fabro-workflow/src/operations/start.rs`
- `lib/crates/fabro-workflow/src/operations/resume.rs`
- `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- `lib/crates/fabro-workflow/src/lifecycle/disk.rs`
- CLI administrative flows that directly mutate status:
  - `lib/crates/fabro-cli/src/commands/runs/rm.rs`
  - `lib/crates/fabro-cli/src/commands/run/rewind.rs`

### 2. Enrich `checkpoint.completed` to carry a full checkpoint snapshot

Current `checkpoint.completed` is not enough to rebuild `Checkpoint`.

Add fields covering:

- `timestamp` is still the envelope `ts`
- `current_node`
- `completed_nodes`
- `node_retries`
- `context_values`
- `node_outcomes`
- `next_node_id`
- `git_commit_sha`
- `loop_failure_signatures`
- `restart_failure_signatures`
- `node_visits`
- `diff`

Emitter seam:

- `lib/crates/fabro-workflow/src/lifecycle/event.rs`

Producer seam for the source checkpoint object:

- `lib/crates/fabro-workflow/src/lifecycle/disk.rs`

Design rule:

- one `checkpoint.completed` event must be sufficient to reconstruct one historical checkpoint record without replaying prior stage events

That keeps checkpoint history export and `rebuild_meta` simple.

Do not split checkpoint reconstruction back across `stage.completed` and other incidental events unless there is a strong size or performance reason. A saved checkpoint is a first-class domain event and should be self-contained.

### 3. Enrich `pull_request.created` to carry the full record

Current event payload is too small for `PullRequestRecord`.

Add:

- `html_url`
- `number`
- `owner`
- `repo`
- `base_branch`
- `head_branch`
- `title`
- `draft`

Producer seam:

- `lib/crates/fabro-workflow/src/pipeline/pull_request.rs`

After this lands, `put_pull_request` should become removable during the later memoized-state cutover.

Do not add a second storage-oriented PR event. The semantic event is already "pull request created"; it just needs the full payload.

### 4. Enrich `run.completed` with the final patch

Current final patch only exists via direct store writes.

Do not add a separate storage-shaped event. The final patch is run-level terminal metadata, so it belongs on the terminal success event.

Enrich `run.completed`, using the patch already computed from:

- `lib/crates/fabro-workflow/src/lifecycle/git.rs`

Required payload:

- `final_patch: Option<String>`

Projection rule:

- `RunState.final_patch` projects from `run.completed.properties.final_patch`

This keeps final run summary data in one place alongside:

- `status`
- `duration_ms`
- `artifact_count`
- `final_git_commit_sha`

If failed runs later need final patch coverage too, extend the terminal failure event deliberately. Do not introduce a separate patch-persistence event unless terminal events prove insufficient.

### 5. Finish provider-used coverage using existing handler-specific events

The current system still reads `provider_used.json` from disk and syncs it into the store. That is not event-sourced.

Replace that with one explicit projection rule based on existing handler-specific events.

Use:

- `stage.prompt` for prompt-mode stages
- forwarded `agent.session.started` for agent-mode stages
- `agent.cli.started` for CLI-backed agent stages

If agent-mode forwarded session events still do not carry enough metadata, enrich `AgentEvent::SessionStarted` rather than adding a new stage-wide event.

Required projected output:

- `mode`
- `provider`
- `model`
- any existing raw provider-used JSON fields that are still needed by consumers

Likely seams:

- `lib/crates/fabro-workflow/src/handler/agent.rs`
- `lib/crates/fabro-workflow/src/handler/llm/api.rs`
- `lib/crates/fabro-workflow/src/pipeline/retro.rs` if retro uses the same forwarded agent session path
- any CLI-backed LLM path if it still produces `provider_used.json`

Do not keep the current “read JSON sidecar, then `put_node_provider_used`” pattern once this event exists.

Do not add a stage-generic provider-used event. Provider metadata is transport-specific and should stay attached to the prompt/agent/CLI events that actually know it.

### 6. Enrich `parallel.completed` with the final results payload

`parallel.branch.completed.head_sha` is useful but not enough to replace `put_node_parallel_results`.

Use the existing terminal parallel event rather than adding a storage-shaped event name.

Add to `parallel.completed`, emitted from:

- `lib/crates/fabro-workflow/src/handler/parallel.rs`

Required new payload:

- `results` as the same JSON array currently persisted to `parallel_results.json`

Projection rule:

- `StageProjection.parallel_results` projects from `parallel.completed.properties.results`

Why this is the right event:

- it is emitted after all branch executions have joined
- the final result set has already been assembled
- it represents completion of the parallel node’s branch-collection phase

The workflow-level outcome of the node still comes from `stage.completed`; `parallel.completed` just becomes the canonical source for the branch result set.

Do not add `parallel.results_recorded` or similar. The semantic event already exists.

### 7. Lock down the node diff rule

We already added `checkpoint.completed.diff`, but the memoized plan should not proceed until there is one explicit derivation rule for node diff.

Decision required:

- either `StageProjection.diff` is “latest checkpoint diff for that node visit”
- or add a dedicated `node.diff_generated` event

This follow-up plan should pick one and update docs/tests accordingly.

Given the current code, using `checkpoint.completed.diff` is the simpler option unless multiple diffs per node visit need to be preserved.

Prefer `checkpoint.completed.diff` unless a concrete consumer proves that diff generation and checkpoint persistence are semantically different moments.

## File Map

Likely files to touch:

- `docs-internal/events.md`
- `docs-internal/run-directory-keys.md`
- `docs-internal/events-strategy.md`
- `lib/crates/fabro-workflow/src/event.rs`
- `lib/crates/fabro-workflow/src/lifecycle/event.rs`
- `lib/crates/fabro-workflow/src/lifecycle/disk.rs`
- `lib/crates/fabro-workflow/src/lifecycle/git.rs`
- `lib/crates/fabro-workflow/src/operations/create.rs`
- `lib/crates/fabro-workflow/src/operations/start.rs`
- `lib/crates/fabro-workflow/src/operations/resume.rs`
- `lib/crates/fabro-workflow/src/pipeline/finalize.rs`
- `lib/crates/fabro-workflow/src/pipeline/pull_request.rs`
- `lib/crates/fabro-workflow/src/handler/agent.rs`
- `lib/crates/fabro-workflow/src/handler/parallel.rs`
- `lib/crates/fabro-cli/src/commands/runs/rm.rs`
- `lib/crates/fabro-cli/src/commands/run/rewind.rs`
- tests in `fabro-workflow`, `fabro-cli`, and `fabro-store`

## Phases

### Phase 1: Define the missing event contract

- add semantic run lifecycle events
- enrich `checkpoint.completed`
- enrich `pull_request.created`
- enrich `run.completed` with `final_patch`
- add provider-used event coverage
- add parallel-results event coverage
- document the node diff derivation rule

This phase is complete when every row in the follow-up coverage matrix is backed by an explicit event contract.

Priority during this phase:

- first enrich existing semantic events
- only add a truly new event when no existing semantic event owns the data

### Phase 2: Emit the new events everywhere status/data currently writes directly

Replace silent state mutation with canonical event emission first.

Important rule:

- do not remove direct store writes yet
- dual-write is acceptable in this phase
- the purpose is to prove event completeness before the memoized-state migration begins

### Phase 3: Add parity tests against current stored records

Add tests that build real event sequences and verify the future projector contract is now possible for:

- status reconstruction
- start record reconstruction
- checkpoint reconstruction
- checkpoint history reconstruction
- pull request reconstruction
- final patch reconstruction
- provider-used reconstruction
- parallel-results reconstruction

Where a direct legacy store record still exists, compare the event-derived value against the legacy persisted value.

Keep the tests shaped around semantic events, not internal store APIs. The point is to prove the event contract is sufficient.

Minimum parity scenarios:

- create-only run before execution starts
- normal started/running run
- resumed run
- rewound run
- successful git-backed run with final patch
- PR-producing run
- parallel run with branch results
- retro-enabled run
- failed run that exits through terminal/drop-guard paths

### Phase 4: Update the downstream migration plan

Once this follow-up plan lands:

- update `~/.claude/plans/memoized-pondering-knuth.md`
- remove any rows that were intentionally retained as non-event concerns
- mark the newly-completed event-backed rows as ready
- delete stale “likely needs to be added” wording that is no longer true

This keeps the later store-migration plan honest and implementation-ready.

## Verification

1. `cargo build --workspace`
2. `cargo clippy --workspace -- -D warnings`
3. `cargo nextest run -p fabro-workflow`
4. `cargo nextest run -p fabro-cli`
5. `cargo nextest run -p fabro-store`
6. `cargo nextest run --workspace`
7. Manual:
   - create a run and inspect `progress.jsonl`
   - verify semantic run lifecycle events appear in the expected order
   - verify a run with git changes emits `run.completed.properties.final_patch`
   - verify a PR-producing run emits full PR metadata in `pull_request.created`
   - verify a parallel run emits canonical final results on `parallel.completed` without reading `parallel_results.json`

## Exit Criteria

This follow-up plan is complete when:

- every row in the follow-up coverage matrix is either event-backed or explicitly retained as non-event
- direct store writes are no longer the only source for status, checkpoint history, pull request record, final patch, provider-used, or parallel results
- projector parity tests prove the later `RunState` refactor has the event data it needs
- `memoized-pondering-knuth.md` can be updated to proceed without hidden event-contract gaps

At that point, the later migration work should mostly be mechanical projection and API cleanup, not more event-model design.
