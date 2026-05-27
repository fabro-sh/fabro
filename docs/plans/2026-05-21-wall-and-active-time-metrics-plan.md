---
title: "feat: Wall and active time metrics"
type: feature
status: active
date: 2026-05-21
---

# feat: Wall and active time metrics

## Summary

Rename runtime duration concepts from ambiguous duration/runtime/elapsed fields
to explicit wall-time fields, then add first-class active timing.

Definitions:

- `wall_time_ms`: elapsed clock time from start to finish.
- `inference_time_ms`: Fabro-observed LLM request/stream elapsed time.
- `tool_time_ms`: tool or command execution elapsed time.
- `active_time_ms`: `inference_time_ms + tool_time_ms`.

This is greenfield API churn. Do not preserve old public run/stage timing
fields, aliases, or compatibility shims for `duration_ms`, `runtime_secs`, or
`elapsed_secs` on run/stage runtime surfaces.

Run-level active time is total work performed: sum active timing across stage
visits. Parallel work is summed, so run active time can exceed run wall time.

## Key Changes

- Add a shared timing value object in `fabro-types` for stage/run active timing:
  - `wall_time_ms`
  - `inference_time_ms`
  - `tool_time_ms`
  - serialized `active_time_ms` derived by constructors/accessors from
    `inference_time_ms + tool_time_ms`; do not let callers supply an
    independent active-time value.
- Replace run/stage public timing fields:
  - stage/run terminal event props use `wall_time_ms` plus the active timing
    breakdown.
  - `StageProjection` stores the timing breakdown instead of stage
    `duration_ms`.
  - `RunTimestamps` keeps timestamps only; move elapsed values into a separate
    run timing object.
  - `/runs/{id}/stages` and `/runs/{id}/billing` expose timing in milliseconds,
    not `runtime_secs`.
- Keep `duration_ms` only for unrelated subsystem-specific operational events
  where the name is still local and unambiguous, such as sandbox setup,
  metadata snapshot, setup commands, and hook execution. The cleanup
  target is public run/stage runtime semantics.
- Update OpenAPI and regenerate the Rust and TypeScript API clients after
  schema edits.

## Timing Behavior

- `prompt` nodes:
  - inference = elapsed time spent in the one-shot LLM backend call.
  - tool = 0.
- native `agent` nodes:
  - inference = sum of elapsed time spent opening/consuming LLM streams for new
    turns in the stage.
  - tool = sum of elapsed time spent executing agent tool calls.
  - retry backoff and waiting for steering are wall time, not active time.
- opaque external/ACP agent nodes:
  - inference = 0 for v1 because Fabro cannot reliably separate model time from
    process runtime.
  - tool = external agent process wall time.
- `command` nodes:
  - inference = 0.
  - tool = command wall time from the sandbox command result.
- `human`, `wait`, `conditional`, `fan-in`, `start`, and `exit`:
  - inference = 0.
  - tool = 0.
- `parallel` container nodes:
  - active = 0 on the container stage.
  - child/branch stages carry work timing so rollups do not double count.

## Implementation

- In `fabro-types`, introduce the timing structs and replace the relevant fields
  in `Outcome`, `NodeResult` consumers, `StageProjection`, `Conclusion`,
  `RunTimestamps`, `RunCompletedProps`, `RunFailedProps`,
  `StageCompletedProps`, `StageFailedProps`, `RunBillingStage`, and
  `RunBillingTotals`.
- In `fabro-workflow`, rename run/stage execution fields from `duration_ms` to
  `wall_time_ms` and thread timing through lifecycle events, terminal events,
  conclusion building, pull request summaries, timeline/billing rollups, and
  test support fixtures.
- In `fabro-agent`, add timing data to agent events or session results so
  `fabro-workflow` can aggregate:
  - LLM stream/request elapsed time per assistant response.
  - tool call elapsed time per tool completion.
  - preserve token billing behavior separately from timing.
- In `fabro-store`, update event projection to write stage `started_at`, timing
  breakdowns, and run summary timing from the new event props.
- In `fabro-server`, replace runtime billing aggregation with a timing rollup
  owned by workflow/projection code. Billing endpoints may include timing, but
  billing logic should not define timing semantics.
- In `apps/fabro-web`, update run list/detail/stages/billing views and tests to
  render wall time and active time from the new fields.
- Remove all run/stage public API references to old timing names from
  `docs/public/api-reference/fabro-api.yaml` and regenerated clients.

## Test Plan

- `fabro-types`:
  - run and stage event round trips serialize the new timing payloads.
  - old public run/stage timing properties are absent from serialized fixtures.
  - API-facing timing structs round trip through generated schemas.
- `fabro-store`:
  - `stage.started` records `started_at`.
  - stage terminal events store `wall_time_ms` and active breakdowns.
  - run summaries expose timestamp fields and run timing without
    `elapsed_secs`.
  - retried stages reset per-attempt live wall-time state correctly.
- `fabro-workflow`:
  - prompt stages report inference-only active timing.
  - command stages report tool-only active timing.
  - native agent stages sum LLM turn timing and tool timing.
  - human/wait/conditional/fan-in/start/exit stages report zero active timing.
  - parallel stage rollups sum child active work and avoid container double
    counting.
  - repeated node visits sum timing by node in rollups.
- `fabro-server`:
  - `/runs/{id}/stages`, `/runs/{id}/billing`, run detail, and run list return
    new timing fields only.
  - aggregate billing/timing totals sum active work across completed runs.
  - OpenAPI conformance passes after regeneration.
- `apps/fabro-web`:
  - run list/detail/billing/stages render wall time and active time.
  - in-flight wall-time ticking still uses `started_at`.
  - no UI code reads `runtime_secs`, `elapsed_secs`, or run/stage
    `duration_ms`.

## Validation

Run focused checks first:

```bash
cargo nextest run -p fabro-types -p fabro-store -p fabro-workflow -p fabro-server
cd apps/fabro-web && bun test && bun run typecheck
```

Then run full workspace checks before merging:

```bash
cargo build --workspace
cargo nextest run --workspace
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
git diff --check
```

## Assumptions

- Inference time is Fabro-observed LLM request/stream elapsed time, not
  provider-reported model-only compute time.
- LLM retry backoff, queueing outside a request/stream, human waits, steering
  waits, and scheduler gaps are wall time but not active time.
- Active timing is finalized-event based in v1; live active-time ticking can be
  added later if it becomes necessary.
- No compatibility layer is required for existing API clients or stored run
  event data.
