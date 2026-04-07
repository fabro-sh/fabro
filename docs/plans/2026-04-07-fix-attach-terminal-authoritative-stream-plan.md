---
title: "fix: make attach streams terminal-authoritative"
type: fix
status: active
date: 2026-04-07
---

# fix: make attach streams terminal-authoritative

## Overview

Remove `ATTACH_FINAL_STATUS_GRACE` by making `GET /api/v1/runs/{id}/attach` the authoritative source of terminal completion for `fabro attach`.

After this change:

- the server streams ordered events starting at `since_seq`
- if a terminal event is reached, the server emits it and closes the stream immediately
- the CLI exits as soon as it sees `run.completed` or `run.failed`
- the CLI no longer waits for quiet time or polls run state after attach completes

## Problem Frame

Current behavior is split across the server and CLI in a way that creates a completion race:

- the server treats `/attach` as a live-only event tail and returns `410 Gone` when the run is no longer live
- the CLI lists historical events first, then opens `/attach?since_seq=next_seq`
- if the run finishes between those two steps, the CLI can miss the terminal event and has to rely on `ATTACH_FINAL_STATUS_GRACE`
- even when the CLI does receive a terminal event, the server-side live stream does not close on that event, so the CLI waits for a grace period before deciding the stream is done

The desired contract is simpler: `attach` should trust the stream. If the stream ends before a terminal event, that is an error, not a case for client-side repair logic.

## Requirements Trace

- R1. `GET /api/v1/runs/{id}/attach` must serve ordered events beginning at `since_seq`, whether the run is still live or already terminal.
- R2. If the stream reaches `run.completed` or `run.failed`, the server must emit that event and close the stream immediately.
- R3. `fabro attach` must derive completion and exit status from streamed events, not from grace periods or follow-up state polling.
- R4. `GET /api/v1/runs/{id}/attach` must stop returning `410 Gone` for completed runs.
- R5. The existing bare-attach API behavior when `since_seq` is omitted remains unchanged: the endpoint starts at the current tail, so attaching after a completed run with no unread events may produce an empty stream that closes immediately.
- R6. The CLI must not interpret R5 as a protocol success path. `fabro attach` still short-circuits terminal runs via its initial state-and-history replay, so its premature-EOF error handling applies only to the live-stream path entered after the initial terminal check.
- R7. The run-specific SSE attach stream must send keepalives during idle periods so long-running quiet stages do not surface as transport EOFs.
- R8. Interview handling and Ctrl-C detach behavior remain unchanged.

## Key Technical Decisions

- The server owns the completion contract.
  The CLI stays simple and exits immediately on terminal events.

- Premature EOF is a protocol error.
  If the live attach stream ends before a terminal event is observed, `fabro attach` exits non-zero with a clear error instead of performing a repair lookup. This does not conflict with R5 because the CLI does not enter the live-stream path for already-terminal runs.

- `/attach` becomes replay-capable for completed runs.
  The endpoint no longer uses “run is live” as a gate for whether unread events can be served.

- No new durable storage primitive is required, but the live-watch helper must be hardened.
  `open_run_reader()` already returns the shared active `RunDatabase` for live runs, but the current `watch_events_from()` handoff is not strong enough for this contract because it snapshots recent events before subscribing. The implementation must make the replay-to-watch transition seq-complete.

- Keepalive is in scope for run-specific attach.
  The new fail-fast EOF contract is only acceptable if the run-specific SSE stream gets the same idle keepalive treatment as the existing global attach endpoint.

## Public API / Interface Changes

- Update `docs/api-reference/fabro-api.yaml` for `GET /api/v1/runs/{id}/attach`:
  - change the description from “live run” SSE to “ordered event stream starting at `since_seq`, replaying persisted events and continuing with live updates while the run remains active”
  - remove the `410 Run is not live on this server` response

- Regenerate generated clients after the spec update:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`

- Remove the CLI’s `RunAttachStreamError::Gone` branch from the Rust server client.

## Implementation Units

### [ ] Unit 1: Make server attach replay terminal-safe

**Goal**

Change the server attach endpoint so it can always deliver unread terminal events and self-close on terminal completion.

**Files**

- `lib/crates/fabro-server/src/server.rs`
- `lib/crates/fabro-store/src/slate/run_store.rs`
- `docs/api-reference/fabro-api.yaml`

**Approach**

- Replace the current “live run only” guard in `attach_run_events()`.
- Open the run reader first and return `404` only when the run truly does not exist.
- Compute `start_seq` exactly as today.
- Replay persisted events from `start_seq` using `list_events_from_with_limit` in bounded batches, converting them through the existing `sse_event_from_store()` helper.
- Detect terminal events during replay. If replay emits `run.completed` or `run.failed`, close the SSE stream immediately after that event.
- Before using live watch for attach, harden `watch_events_from()` so it is seq-complete for handoff:
  - subscribe to `event_tx` before snapshotting `recent_events`
  - snapshot and emit cached events at or after the requested seq
  - then drain broadcast events, discarding any seq lower than the next expected seq
  This closes the snapshot-to-subscribe gap that exists in the current helper.
- If replay reaches the current tail without hitting a terminal event and the run is still active, switch to the hardened `watch_events_from(next_seq)` on the shared `RunDatabase`.
- In the live phase, stream events until the first terminal event, then close immediately.
- Add `.keep_alive(KeepAlive::default())` to the run-specific SSE response so idle stages do not turn into transport EOFs.

**Patterns to follow**

- Existing event serialization helpers in `lib/crates/fabro-server/src/server.rs`
- Existing event listing behavior in `list_run_events()`
- Global attach keepalive behavior in `lib/crates/fabro-server/src/server.rs`
- Updated `watch_events_from()` semantics in `lib/crates/fabro-store/src/slate/run_store.rs`

**Test scenarios**

- Attach to a live run and confirm the stream includes stage events, then a terminal event, then EOF.
- Attach after the run has already completed with `since_seq` before the terminal event and confirm replay includes the terminal event, then EOF.
- Attach after completion with `since_seq` after the last event and confirm the stream returns `200` and closes cleanly with no events.
- Attach during the race where the run completes after the client reads history but before it opens `/attach`, and confirm the terminal event is still delivered.
  Use a barrier or oneshot to hold terminal completion until after the history read returns and release it before the `/attach` request starts, so the race is deterministic rather than sleep-based.
- Attach to a nonexistent run and confirm `404` remains unchanged.
- Attach to a run with a deliberately quiet stage and confirm the SSE response stays open via keepalive until later events arrive.

**Verification**

- Server tests prove `200` is returned for completed runs with unread events.
- Server tests prove the stream terminates immediately after a terminal event.

### [ ] Unit 2: Simplify CLI attach around terminal events

**Goal**

Remove timer-based completion handling from `fabro attach` and make the command trust streamed terminal events.

**Files**

- `lib/crates/fabro-cli/src/commands/run/attach.rs`
- `lib/crates/fabro-cli/src/server_client.rs`

**Approach**

- Delete `ATTACH_FINAL_STATUS_GRACE`.
- Delete `determine_exit_code_with_server()`.
- Remove `RunAttachStreamError::Gone` and its special handling.
- Keep the initial replay path: if the initial event list or run state already shows the run is terminal, replay and exit as today.
- In the live attach path, emit events and return immediately when `event_exit_code()` sees `run.completed` or `run.failed`.
- If the live attach stream ends before a terminal event is observed, return a non-zero protocol error instead of polling server state.
- Document in code that this EOF rule applies only after the command has already ruled out the R5 completed-run replay case via the initial terminal check.
- Leave interview prompting and Ctrl-C behavior unchanged.

**Patterns to follow**

- Existing `event_exit_code()` extraction in `lib/crates/fabro-cli/src/commands/run/attach.rs`
- Existing replay behavior in `replay_run_with_client()`

**Test scenarios**

- Successful attach on a live run exits `0` immediately after `run.completed`.
- Failed attach on a live run exits `1` immediately after `run.failed`.
- Completed run replay still exits with the correct code without opening a live stream.
- Premature EOF before any terminal event produces a non-zero exit and clear error text.

**Verification**

- CLI tests no longer depend on grace-period timing.
- No attach code path polls run state after a live stream finishes.

### [ ] Unit 3: Align tests, demo behavior, and generated API artifacts

**Goal**

Update repo expectations so they match the new terminal-authoritative attach contract.

**Files**

- `lib/crates/fabro-server/tests/it/scenario/run_completion.rs`
- `lib/crates/fabro-server/tests/it/scenario/sse.rs`
- `lib/crates/fabro-cli/tests/it/cmd/attach.rs`
- `lib/crates/fabro-server/src/demo/mod.rs`
- generated Rust and TypeScript API artifacts

**Approach**

- Replace current `200 or 410` attach assertions with `200`-only expectations where applicable.
- Update the server unit/integration test near the current `StatusCode::GONE` assertion to verify replay-and-close semantics instead.
- Update the demo attach stub to return a short SSE response that ends cleanly, rather than `410`.
- Regenerate Rust and TypeScript clients after the OpenAPI change.

**Test scenarios**

- Server scenario tests verify completed-run attach no longer returns `410`.
- CLI mock-server attach tests verify the command exits from streamed terminal events rather than fallback logic.
- Demo-mode attach still behaves coherently for callers expecting an attach response.

**Verification**

- Spec, generated clients, and tests all describe the same `attach` contract.

## Test Plan

- `cargo nextest run -p fabro-server`
- `cargo nextest run -p fabro-cli`
- Target the attach-specific server and CLI tests first while iterating, then run the crate suites before landing.

## Assumptions

- `fabro logs` and `FOLLOW_TERMINAL_GRACE` are out of scope for this change.
- Every valid terminal run should emit a persisted terminal event (`run.completed` or `run.failed`); any gap found during implementation should be treated as a server bug to fix, not a reason to reintroduce client grace timing.
- The user preference for attach simplicity is authoritative: premature EOF is an error, not a repairable condition.
