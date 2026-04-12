# Subprocess Run Workers And Signal Control

## Summary
- Move workflow execution out of `fabro server` into one hidden worker subprocess per run.
- Use Unix signals for lifecycle control, but keep the server as the sole durable writer for run events, projections, and summaries.
- Stream canonical worker events back to the server over the worker stdout pipe so the event log, SSE, and API status stay coherent without multi-process append races.

## Scope Boundaries
In scope:
- replace in-process run execution with supervised worker subprocesses
- repurpose hidden `fabro __runner` into hidden `fabro __run-worker`
- use signals for `cancel`, `pause`, and `unpause`
- make the server the only writer to a run's durable event stream
- define a worker spawn contract, stdout/stderr contract, and control priority rules
- add request and effect events for run control
- phase delivery so subprocess cancel lands before pause and procline polish

Out of scope:
- non-Unix parity for worker supervision
- crash-time worker reattachment in the first delivery
- changing the current public control routes away from `/runs/{id}/cancel`, `/pause`, and `/unpause`
- changing terminal cancellation away from durable `status=failed` with `status_reason=cancelled`

## Problem Frame
The intended architecture is server-supervised subprocess workers, not server-local async tasks. The current plan also assumed both server and worker could append to the same run event stream, but the current `RunDatabase` writer is process-local: sequence allocation, event cache, and watch fanout are held in per-process memory. That makes multi-process writes and cross-process `watch_events_from()` unsafe as a foundation for this refactor.

The revised plan therefore needs to solve four things explicitly:

- control delivery without shared in-process primitives
- durable event ordering without multi-process writers
- server observation of worker state transitions
- clear priority and override rules when multiple control requests arrive

## Key Decisions
- Hidden worker command
  - Rename hidden `fabro __runner` to hidden `fabro __run-worker`.
  - `__run-worker` executes a single run locally and does not call back into the server API.
- Single-writer rule
  - The server is the only process that appends durable run events, updates run projections, and drives SSE.
  - The worker must not open a write-capable `RunDatabase`.
  - The worker may open the run store read-only for manifest and checkpoint reads only.
- Worker-to-server event path
  - Worker stdout is reserved for newline-delimited canonical `RunEvent` JSON objects.
  - The worker builds canonical events once, using the existing event model, and writes them to stdout.
  - The server reads stdout, validates each event, appends it to the run store, and fans it out to SSE and any in-process listeners.
  - This replaces the current assumption that the worker writes directly to the run store.
- Worker stderr and logs
  - Worker stderr is not part of the event stream.
  - The server captures worker stderr, writes it to a per-run log file under the run scratch directory, and mirrors lines into server tracing with the run id attached.
- Worker spawn contract
  - The server spawns `fabro __run-worker --run-id <id> --mode <start|resume> --storage-dir <dir>`.
  - If the active config path is required to reproduce local runtime behavior, pass `--config <path>` as well.
  - The worker does not need a server address because it does not talk HTTP to the server.
- Control delivery
  - `cancel` sends `SIGTERM` to the worker process.
  - After the grace timeout, the server sends `SIGKILL` to the worker process group.
  - `pause` sends `SIGUSR1`.
  - `unpause` sends `SIGUSR2`.
  - Do not use `SIGSTOP` / `SIGCONT` for API pause and unpause.
- Control priority and conflict rules
  - Priority is `cancel > pause > unpause`.
  - `pending_control` remains a single value and later accepted requests overwrite lower-priority pending requests.
  - `cancel` is accepted from `submitted`, `queued`, `starting`, `running`, or `paused`.
  - A `cancel` request overwrites a pending `pause` or `unpause`.
  - `pause` is accepted only when observed status is `running` and `pending_control` is `null`.
  - `unpause` is accepted only when observed status is `paused` and `pending_control` is `null`.
  - A lower-priority request while a higher-priority request is pending returns `409`.
  - If `pause` is pending and `cancel` arrives before the worker reaches a safe point, the worker must skip `run.paused` and terminate through the normal cancel path.
  - If the run is already paused and `cancel` arrives, the worker must exit the pause wait and cancel immediately.
- Signal handling in the worker
  - `SIGTERM` requests cooperative cancellation.
  - `SIGUSR1` requests cooperative pause.
  - `SIGUSR2` requests cooperative unpause.
  - Pause takes effect only at safe points such as between stages, before retry sleeps, before entering or resuming human waits, and at existing cancellation checkpoints.
- Event naming
  - Use `run.cancel.requested`, `run.pause.requested`, and `run.unpause.requested` for accepted control requests.
  - Use `run.paused` and `run.unpaused` for observed worker transitions.
  - Keep terminal cancellation on `run.failed` with `reason=cancelled`.
  - Avoid `run.resumed` because `resume` already means resume-from-checkpoint elsewhere in the system.
- Restart behavior in the first delivery
  - Do not attempt PID-based worker reattachment in the first delivery.
  - On graceful server shutdown, terminate active workers before exit.
  - On server startup, any non-terminal run with stale worker metadata from a prior server process is marked interrupted or terminated and its worker metadata is cleared.
  - Robust crash-time reattachment is deferred to a later phase and must include process identity verification before any signal delivery.
- API shape
  - Keep the current control routes and verbs.
  - Extend `RunStatusResponse` with `status_reason` and `pending_control`.
  - Extend durable `StoreRunSummary` with `pending_control`.
  - Control endpoints return the current observed status plus `pending_control`; they do not report the requested action as completed until the worker emits the corresponding effect event.
- Process titles
  - Server titles:
    - `fabro server boot`
    - `fabro server unix:/path/to/socket`
    - `fabro server tcp:127.0.0.1:3000`
    - `fabro server stopping`
  - Worker titles use the existing 12-character short run-id convention:
    - `fabro <short-id> start`
    - `fabro <short-id> resume`
    - `fabro <short-id> init`
    - `fabro <short-id> running`
    - `fabro <short-id> waiting`
    - `fabro <short-id> paused`
    - `fabro <short-id> cancelling`
    - `fabro <short-id> succeeded`
    - `fabro <short-id> failed`
    - `fabro <short-id> cancelled`

## Delivery Plan
### Phase 1: Subprocess execution and signal cancel
- Spawn one worker subprocess per started run.
- Make the server the sole event-store writer.
- Stream canonical worker events over stdout into the server.
- Route worker stderr into per-run log files and server tracing.
- Implement signal-based `cancel` only.
- Do not support crash-time worker reattachment in this phase.

### Phase 2: Control request events and API status enrichment
- Add `run.cancel.requested` and `pending_control`.
- Extend `RunStatusResponse` and `StoreRunSummary` with `status_reason` and `pending_control`.
- Update status projections so accepted cancel requests surface immediately without pretending the run has already terminated.

### Phase 3: Pause and unpause
- Add `SIGUSR1` and `SIGUSR2` handling.
- Add `run.pause.requested`, `run.unpause.requested`, `run.paused`, and `run.unpaused`.
- Implement the priority and overwrite rules defined above.

### Phase 4: Procline polish and optional crash-time recovery
- Add title helpers and short-id process titles.
- If crash-time worker recovery is still wanted, design it as a separate pass with explicit worker identity verification before any PID-based signaling.

## Implementation Changes
### 1. Server supervision and worker pipes
- Replace the current `tokio::spawn(execute_run(...))` path in `lib/crates/fabro-server/src/server.rs` with worker subprocess spawning.
- Introduce a server-side supervisor record for live runs that stores:
  - observed status
  - created time
  - local error text
  - worker PID
  - worker PGID
  - worker mode (`start` or `resume`)
  - current `pending_control`
  - handles for worker stdout and stderr tasks
- On worker spawn:
  - set process-group isolation
  - capture stdout and stderr
  - start one task that parses stdout into canonical `RunEvent`s and appends them to the run store
  - start one task that drains stderr into a per-run log file and tracing
  - set observed status to `starting`
- On worker exit:
  - if the worker already produced a terminal run event, clear live worker metadata only
  - if no terminal run event was appended, append a terminal failure with `reason=terminated`

### 2. Workflow engine event sink refactor
- Refactor run execution so the worker path no longer depends on direct run-store writes from inside `fabro-workflow`.
- Introduce a run-event sink abstraction used by workflow execution, retro, pull-request creation, and any remaining bypass paths that currently append directly to the run store.
- For worker execution, the sink serializes canonical `RunEvent` JSON lines to stdout.
- Preserve the current event-strategy rule that the canonical `RunEvent` is built exactly once and reused for all downstream sinks.

### 3. Worker execution and safe-point control
- Rewrite `lib/crates/fabro-cli/src/commands/run/runner.rs` into the `__run-worker` entrypoint that:
  - loads the run manifest and checkpoint state read-only from storage
  - executes `operations::start` or `operations::resume`
  - owns the workflow `Emitter`
  - translates OS signals into local cooperative control flags
- At safe points:
  - if cancel is requested, terminate through the existing cancellation path
  - if pause is requested and cancel is not pending, emit `run.paused`, block until unpause or cancel, then emit `run.unpaused` when execution continues
- Make cancel override a paused state immediately.

### 4. Event model and projection updates
- Extend `fabro-workflow` event types and `fabro-types` `EventBody` with:
  - `run.cancel.requested`
  - `run.pause.requested`
  - `run.unpause.requested`
  - `run.paused`
  - `run.unpaused`
- Keep request events server-originated and effect events worker-originated.
- Update `lib/crates/fabro-store/src/run_state.rs` so projections:
  - track `pending_control`
  - do not let request events overwrite observed status
  - apply `run.paused` and `run.unpaused` to observed status
  - continue projecting terminal cancellation as `status=failed` and `status_reason=cancelled`
- Extend durable `RunSummary` with `pending_control`.

### 5. API and client updates
- Update `docs/api-reference/fabro-api.yaml` with:
  - a new `RunControlAction` schema using `cancel`, `pause`, and `unpause`
  - `pending_control` on `RunStatusResponse`
  - `status_reason` on `RunStatusResponse`
  - `pending_control` on `StoreRunSummary`
- Define control endpoint behavior as:
  - validate whether the action is currently allowed
  - apply the priority and overwrite rules above
  - append the corresponding request event
  - update `pending_control`
  - send the signal if a live worker exists
  - return the current observed status response
- Regenerate both generated API clients after the OpenAPI change.

### 6. Process title cleanup
- Keep using `fabro_proc::title_init()` and `fabro_proc::title_set()`.
- Add helpers for server title updates by bind and lifecycle phase, and worker title updates by short run id and worker phase.
- Keep procline verification lightweight; do not build brittle exact-string end-to-end assertions around process titles.

## Test Plan
- Single-writer and event-path tests
  - worker execution path opens the run store read-only only
  - worker stdout emits valid newline-delimited canonical `RunEvent` payloads
  - server appends worker-streamed events in order and SSE reflects the appended stream
  - worker stderr is captured into the per-run log file
- Cancel tests
  - starting a queued run spawns a worker subprocess and records PID and PGID
  - `POST /runs/{id}/cancel` appends `run.cancel.requested`, sets `pending_control=cancel`, sends `SIGTERM`, and later converges to durable `failed` with `status_reason=cancelled`
  - cancelling a submitted or queued run reaches durable `failed/cancelled` without spawning a worker
  - an unresponsive worker is escalated from worker `SIGTERM` to process-group `SIGKILL`
- Pause and unpause tests
  - `POST /runs/{id}/pause` on a running worker appends `run.pause.requested`, sets `pending_control=pause`, and later projects `paused`
  - `POST /runs/{id}/unpause` on a paused worker appends `run.unpause.requested`, sets `pending_control=unpause`, and later projects `running`
  - pause followed by cancel before a safe point never produces `run.paused`
  - cancel while paused exits the pause wait and converges to durable `failed/cancelled`
  - lower-priority actions while a higher-priority action is pending return `409` and append no request event
- Startup behavior tests
  - graceful server shutdown terminates active workers
  - startup clears stale worker metadata and marks prior non-terminal runs interrupted or terminated
- Verification commands
  - `cargo nextest run -p fabro-server`
  - `cargo nextest run -p fabro-workflow`
  - `cargo nextest run -p fabro-store`
  - `cargo nextest run -p fabro-cli`
  - `cargo fmt --check --all`
  - `cargo clippy --workspace -- -D warnings`

## Assumptions
- This refactor is Unix-first and may explicitly reject or defer non-Unix worker supervision behavior.
- The first delivery optimizes for correct subprocess supervision and durable event ordering, not crash-time worker survival across server restarts.
- `pause` and `unpause` are cooperative safe-point transitions, not immediate OS-level stop and continue.
- The short run id remains the first 12 characters of the ULID, matching current CLI presentation.
