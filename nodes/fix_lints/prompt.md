Goal: # Focused `Blocked` Run Status Plan

## Summary

- Add `Blocked` as a first-class run lifecycle status for runs that are waiting on external intervention.
- Keep `Paused` separate. `Paused` is operator intent; `Blocked` is an execution condition.
- Keep existing engine concepts in scope: `Succeeded` and `Dead` remain real statuses in this pass.
- Make `/api/v1/runs`, `/api/v1/runs/{id}`, mutation responses, and `/api/v1/runs/{id}/state` use one truthful operator vocabulary.
- Keep `/api/v1/boards/runs` explicitly lossy and web-optimized.
- Add `BlockedReason`, starting with `human_input_required`.
- Add explicit lifecycle events for `run.queued`, `run.blocked`, and `run.unblocked`.
- No alerting/email work in this pass.

## Scope And Decisions

### Canonical Operator Status Vocabulary

Use one shared run status vocabulary across the durable projection, operator APIs, generated clients, and CLI:

- `submitted`
- `queued`
- `starting`
- `running`
- `blocked`
- `paused`
- `removing`
- `succeeded`
- `failed`
- `dead`

Additional decisions:

- `cancelled` remains `failed` plus `status_reason=cancelled`; it is not a new top-level run status in this pass.
- `status` becomes required/non-null on operator-facing surfaces.
- If a run exists but the projection has no lifecycle status yet, synthesize `submitted` rather than returning `null`.
- `/api/v1/boards/runs` remains a derived UI projection and does not need to preserve the full operator vocabulary.
- This is an accepted breaking contract change. The app is greenfield with no prod installs, so do not add versioning, migration work, serde aliases, or compatibility shims for the status-enum changes or `reason -> status_reason` rename.

### `Blocked` Semantics

- `Blocked` means the run cannot proceed until some external condition is resolved.
- In this pass the only `BlockedReason` is `human_input_required`, but the enum and event shapes should allow more reasons later.
- `blocked_reason` is a separate field everywhere; do not overload `status_reason`.
- A paused run may still retain `blocked_reason` if the underlying block is unresolved.
- `Paused` wins as the visible status while a run is paused.
- If a blocked run is unpaused and the block is still unresolved, the visible state returns to `blocked` (via the `paused -> running -> blocked` event sequence in Section 2).
- If the block resolves while the run is paused, clear `blocked_reason` and emit `run.unblocked`, but leave `status=paused`.

### Board Contract

`/api/v1/boards/runs` remains a Trello-style projection for the web UI only.

Board columns after this change:

- `initializing`
- `running`
- `blocked`
- `succeeded`
- `failed`

Board mapping rules:

- `submitted`, `queued`, `starting` -> `initializing`
- `running`, `paused` -> `running`
- `blocked` -> `blocked`
- `succeeded` -> `succeeded`
- `failed`, `dead` -> `failed`
- `removing` -> off-board

Additional board decisions:

- Replace the current `waiting` column with `blocked`.
- Replace the older `working | review | merge` board schema entirely. Update OpenAPI `BoardColumn`, server responses, and web `ColumnStatus` types to use only `initializing | running | blocked | succeeded | failed`.
- Keep failed behavior as-is.
- Keep paused runs visually indistinguishable from running in this pass.
- Blocked cards should show the oldest unresolved pending interview question text.
- That question text should be derived only in `/api/v1/boards/runs`, not added to `StoreRunSummary`.
- Known limitation for this pass: a run that is both paused and still blocked appears in the `running` column. A follow-up can add a paused attention indicator or richer board card state.

## Implementation Units

### 1. Shared Types And OpenAPI

Update the shared contract in:

- [docs/api-reference/fabro-api.yaml](/Users/bhelmkamp/p/fabro-sh/fabro/docs/api-reference/fabro-api.yaml)
- [lib/crates/fabro-types/src/status.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/status.rs)
- [lib/crates/fabro-types/src/run_event/mod.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/mod.rs)
- generated Rust client/types under `lib/crates/fabro-api`
- generated TypeScript models under `lib/packages/fabro-api-client/src/models/`

Required changes:

- Collapse OpenAPI `RunStatus` and `InternalRunStatus` into one shared `RunStatus` schema with the canonical operator vocabulary above.
- Add `Queued` and `Blocked` variants to the Rust `RunStatus` enum in `status.rs`. Update `is_active()` to include both (they are incomplete active states). Update `is_terminal()`, `can_transition_to()`, `Display`, and `FromStr` accordingly.
- This is an intentional breaking API change: remove public `completed` and `cancelled`, add public `blocked`, `removing`, `succeeded`, and `dead`, and rename `RunStatusRecord.reason` to `status_reason` with no compatibility layer.
- Add `BlockedReason` schema with initial value `human_input_required`.
- Add `blocked_reason` to:
  - `RunStatusResponse`
  - `RunStatusRecord`
  - `StoreRunSummary`
- Rename `RunStatusRecord.reason` to `status_reason` and keep `blocked_reason` separate.
- Make `StoreRunSummary.status` a non-null `RunStatus` reference instead of `string | null`.
- Keep `status_reason` on responses and summaries.
- Expose `pending_interviews` on the `RunProjection` schema for `/api/v1/runs/{id}/state`.
- Regenerate Rust and TypeScript API clients after the spec update.

### 2. Lifecycle Events And Transition Rules

Add event-backed lifecycle support in:

- [lib/crates/fabro-workflow/src/event.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-workflow/src/event.rs)
- [lib/crates/fabro-types/src/run_event/mod.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/mod.rs)
- [lib/crates/fabro-workflow/src/handler/human.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-workflow/src/handler/human.rs)
- [lib/crates/fabro-workflow/src/operations/start.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-workflow/src/operations/start.rs)
- [lib/crates/fabro-workflow/src/run_control.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-workflow/src/run_control.rs)
- [lib/crates/fabro-types/src/status.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/status.rs)

Add new explicit lifecycle events:

- `run.queued`
- `run.blocked`
- `run.unblocked`

Payload decisions:

- `run.blocked` carries `blocked_reason`.
- `run.unblocked` is a minimal effect event; it does not repeat `blocked_reason`.
- `run.queued` mirrors existing status-transition event style.

Event ordering and rules:

- Emit `run.queued` from the server `start`/`resume` path at the moment the run is inserted into managed queued state. Persist it to the durable run event log there; do not synthesize `queued` later in projection replay.
- Emit `run.started` later, when execution begins.
- Keep `run.starting` and `run.running` as the worker bootstrap/execution transitions.
- `run.blocked` and `run.unblocked` must be durable `run.*` events appended through the normal workflow event sink, not SSE-only notifications and not projection-synthesized state.
- Add a run-scoped blocked-state tracker in the workflow runtime, owned by `StartServices`/`RunSession` in `operations/start.rs` and passed into `HumanHandler` through a new `EngineServices` field such as `blocked_state_tracker: Option<Arc<BlockedStateTracker>>`. The tracker should guard unresolved interview count with a mutex so parallel human stages can safely detect `0 -> 1` and `1 -> 0` transitions.
- On first pending interview (`0 -> 1` unresolved questions), the workflow runtime emits `interview.started` and then appends `run.blocked`.
- While already blocked, additional `interview.started` events do not emit another `run.blocked`.
- On final interview resolution (`1 -> 0` unresolved questions), the workflow runtime emits `interview.completed` or `interview.timeout` or `interview.interrupted` and then appends `run.unblocked`.
- Do not emit `run.unblocked` when a blocked run reaches `failed`, `succeeded`, or `dead`; terminal events end the blocked condition implicitly.

Pause/unpause decisions:

- Keep existing cooperative pause behavior for actively running work.
- In this pass, make pause immediate only when the current visible status is `blocked`.
- For immediate pause from blocked:
  - append `run.pause.requested`
  - append `run.paused` immediately
  - do not emit `run.unblocked`
  - this direct server-appended `run.paused` may race with worker-emitted interview resolution and `run.unblocked`; accept that race in this pass and make projection logic order-insensitive so either ordering converges on the same final paused-or-unblocked state
- For unpause when the underlying human block is still unresolved:
  - append `run.unpause.requested`
  - append `run.unpaused`
  - append `run.blocked`
  - this is explicitly `paused -> running -> blocked`; do not add a direct `paused -> blocked` transition
- For unpause when the underlying block has already resolved:
  - append `run.unpause.requested`
  - append `run.unpaused`
- If the blocked condition resolves while paused:
  - emit the interview resolution event
  - emit `run.unblocked`
  - keep `status=paused`

Transition helper updates in `status.rs`:

- add `submitted -> queued`
- add `queued -> starting`
- add `running -> blocked`
- add `blocked -> running`
- add `blocked -> paused` (immediate pause from blocked)
- preserve `running -> paused`
- preserve `paused -> running`
- preserve non-terminal `-> failed` (including from `blocked`)
- keep `dead` as a real terminal status in this pass

### 3. Durable Projection And Truthful Run APIs

Update durable state and operator-facing API behavior in:

- [lib/crates/fabro-store/src/run_state.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-store/src/run_state.rs)
- [lib/crates/fabro-store/src/types.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-store/src/types.rs)
- [lib/crates/fabro-store/src/slate/mod.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-store/src/slate/mod.rs)
- [lib/crates/fabro-server/src/server.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/server.rs)
- [lib/crates/fabro-server/src/demo/mod.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/demo/mod.rs)

Projection changes:

- Extend `RunStatusRecord` and `StoreRunSummary` (Rust: `RunSummary`) with `blocked_reason`.
- Store `blocked_reason=human_input_required` while blocked on pending human input.
- Preserve `blocked_reason` while the run is paused over an unresolved block.
- Clear `blocked_reason` on `run.unblocked`.
- Clear pending interviews on terminal completion/failure as today.
- Synthesize `submitted` if a run exists but no lifecycle status has been projected yet.

Operator API changes:

- `/api/v1/runs` and `/api/v1/runs/{id}` become truthful operator surfaces.
- Remove the lossy status remap that currently converts:
  - `removing -> running`
  - `succeeded -> completed`
  - `failed(cancelled) -> cancelled`
  - `dead -> failed`
- Expose the canonical operator vocabulary directly on these endpoints.
- Keep `status_reason=cancelled` on failed cancellations.
- Include `blocked_reason` alongside `status_reason` and `pending_control`.
- Because `ManagedRun.status` uses the generated API `RunStatus`, this enum collapse intentionally requires broad match-arm updates throughout `lib/crates/fabro-server/src/server.rs`, `lib/crates/fabro-server/src/demo/mod.rs`, and generated client consumers.
- Return the actual current status from mutation endpoints rather than a target status:
  - `start` returns `queued`
  - `pause` from `blocked` returns `paused`
  - `unpause` back to unresolved human input returns `blocked`
  - cooperative `pause` from `running` still returns `running` with `pending_control=pause` until the worker reaches a pause point

Raw state endpoint changes:

- `/api/v1/runs/{id}/state` remains the raw projection surface.
- Make the schema truthful to the Rust payload by exposing `pending_interviews`.
- Use `RunStatusRecord.status_reason` plus `blocked_reason` there too.

Live managed-run reconciliation:

- Update `update_live_run_from_event()` in `server.rs` for `run.queued`, `run.blocked`, and `run.unblocked`.
- Keep `Blocked` treated as an incomplete active state for shutdown/startup handling in this pass.
- Allow blocked runs to be cancelled through the existing cancel endpoint.
- Update `pause_run` to accept `Blocked` in addition to `Running`, implementing the immediate-pause path (appending `run.paused` directly rather than sending a control signal to the worker).
- Update `should_reconcile_run_on_startup` to include `Blocked` and `Queued`.

### 4. Web Board Projection And UI

Update the web-only board projection in:

- [lib/crates/fabro-server/src/server.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-server/src/server.rs)
- [apps/fabro-web/app/data/runs.ts](/Users/bhelmkamp/p/fabro-sh/fabro/apps/fabro-web/app/data/runs.ts)
- [apps/fabro-web/app/routes/runs.tsx](/Users/bhelmkamp/p/fabro-sh/fabro/apps/fabro-web/app/routes/runs.tsx)
- [apps/fabro-web/app/routes/run-detail.tsx](/Users/bhelmkamp/p/fabro-sh/fabro/apps/fabro-web/app/routes/run-detail.tsx)

Required changes:

- Replace `waiting` with `blocked` in the board projection and UI types.
- Add `queued` and `blocked` to the `RunStatus` type and `runStatusDisplay` record in `apps/fabro-web/app/data/runs.ts` with appropriate labels and colors.
- Update OpenAPI `BoardColumn`, server board responses, and web `ColumnStatus` types to remove `working`, `review`, and `merge`.
- Keep board columns `initializing | running | blocked | succeeded | failed`.
- Map statuses per the board contract above.
- Keep `removing` off-board.
- Keep `paused` in the `running` column with no special indicator in this pass.
- Keep `dead` in the `failed` column on the board.
- Populate board card question text from the oldest unresolved pending interview only in `/api/v1/boards/runs`.
- Implement that by having `list_board_runs` open run readers only for summaries whose mapped board column is `blocked`, inspect `RunProjection.pending_interviews`, and choose the oldest question by earliest `started_at`. Keep `StoreRunSummary` unchanged. This may replay or reload projection state per blocked run during board refresh; that performance profile is acceptable in this pass, and implementers should reuse existing `run_store.state()` / projection-cache behavior where available rather than introducing a new caching layer.
- Do not add question text to `StoreRunSummary`.

Board refresh behavior:

- Preserve the current status-refresh triggers and add the new ones. `STATUS_EVENTS` in `apps/fabro-web/app/routes/runs.tsx` should include:
  - `run.submitted`
  - `run.queued`
  - `run.starting`
  - `run.running`
  - `run.removing`
  - `run.paused`
  - `run.unpaused`
  - `run.blocked`
  - `run.unblocked`
  - `run.completed`
  - `run.failed`
  - `interview.started`
  - `interview.completed`
  - `interview.timeout`
  - `interview.interrupted`

Rationale:

- `run.blocked` and `run.unblocked` cover status changes.
- `interview.*` still need to refresh the board because the displayed oldest unresolved question can change while the run remains blocked.

### 5. CLI Consumers

Update CLI consumers in:

- [lib/crates/fabro-cli/src/server_runs.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/src/server_runs.rs)
- [lib/crates/fabro-cli/src/commands/runs/list.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/src/commands/runs/list.rs)
- [lib/crates/fabro-cli/src/commands/run/wait.rs](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-cli/src/commands/run/wait.rs)

Required changes:

- Treat `/api/v1/runs` as truthful and stop inventing fallback status in `server_runs.rs`.
- Add display/color handling for `Queued` and `Blocked`.
- Keep `Succeeded` as the success exit state for CLI wait behavior in this pass.
- Keep `Dead` as a real displayable terminal state when it is actually present.
- Stop using `Dead` as a synthetic fallback for missing server summary status now that `status` is non-null.
- Continue to show `status_reason=cancelled` for cancellations rather than inventing a `Cancelled` top-level status.

## Test Plan

### Shared Types And Event Model

- `lib/crates/fabro-types/src/run_event/mod.rs`
  - round-trip serialization for `run.queued`, `run.blocked`, and `run.unblocked`
  - `run.blocked` payload includes `blocked_reason`
- `lib/crates/fabro-types/src/status.rs`
  - transition tests for `submitted -> queued`, `running -> blocked`, and `blocked -> running`
  - paused overlay path still flows through explicit event order rather than a direct `paused -> blocked` transition
- `lib/crates/fabro-workflow/src/handler/human.rs`
  - first pending interview emits `interview.started` then durable `run.blocked`
  - second pending interview while already blocked does not emit another `run.blocked`
  - final resolution emits `interview.completed`/`timeout`/`interrupted` then durable `run.unblocked`
- `lib/crates/fabro-workflow/src/operations/start.rs`
  - run-scoped blocked-state tracker emits exactly one `run.blocked` on `0 -> 1` and exactly one `run.unblocked` on `1 -> 0`, including parallel human-stage races

### Durable Projection And Server

- `lib/crates/fabro-store/src/run_state.rs`
  - `run.queued` sets `status=Queued`
  - `run.blocked` sets `status=Blocked` and `blocked_reason=HumanInputRequired`
  - `run.unblocked` while status is `Blocked` clears `blocked_reason` and restores `Running`
  - paused-over-blocked preserves `blocked_reason` while `status=Paused`
  - unpause-to-still-blocked yields `RunUnpaused` followed by `RunBlocked`
  - interview resolution while paused clears `blocked_reason` without changing visible `Paused`
  - missing lifecycle status synthesizes `Submitted`
- `lib/crates/fabro-store/src/slate/mod.rs`
  - list/find summaries expose non-null `status`
  - summaries expose `blocked_reason`
- `lib/crates/fabro-server/src/server.rs`
  - `/api/v1/runs` and `/api/v1/runs/{id}` expose `blocked`, `removing`, `succeeded`, and `dead` directly
  - mutation responses return actual current status
  - blocked runs are cancellable
  - startup/shutdown handling still treats blocked runs as incomplete active work in this pass
  - `/api/v1/runs/{id}/state` includes `pending_interviews`
  - `start`/`resume` append durable `run.queued` when enqueueing
  - board response emits `blocked` column, blocked question text, paused-in-running, removing off-board, and dead-in-failed

### Web UI

- `apps/fabro-web/app/data/runs.test.ts`
  - accepts `blocked`, `queued`, `removing`, `succeeded`, and `dead`
  - removes dependency on `waiting`
- `apps/fabro-web/app/routes/runs.test.tsx`
  - blocked runs render in the `blocked` lane
  - paused runs remain in the `running` lane
  - blocked card shows oldest unresolved question text
  - `STATUS_EVENTS` retains `run.starting` and `run.running` while adding the new blocked/queued events
  - question text refreshes correctly on `interview.*` events without a status change

### CLI

- `lib/crates/fabro-cli/src/commands/runs/list.rs`
  - `Queued` and `Blocked` render with expected labels/colors
  - `Dead` remains renderable when actually returned by the API
- `lib/crates/fabro-cli/src/commands/run/wait.rs`
  - `Succeeded` remains the success exit state
  - `Blocked` is non-terminal and continues waiting
  - no synthetic `Dead` fallback is used for server summary status

## Explicit Non-Goals

- No alerting, email, or notification policy in this pass.
- No new paused indicator on the board in this pass.
- No broader redesign of cooperative pause for actively running work.
- No change to cancellation semantics beyond making blocked runs cancellable and keeping `failed + status_reason=cancelled`.
- No attempt to make blocked runs survive restart as a durable parked state in this pass.


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout:
    ```
    error: use of a disallowed method `std::process::Command::new`
       --> lib/crates/fabro-server/src/serve.rs:504:21
        |
    504 |         let child = std::process::Command::new("bun")
        |                     ^^^^^^^^^^^^^^^^^^^^^^^^^^
        |
        = note: Prefer tokio::process::Command on Tokio paths; document intentional synchronous subprocesses with #[expect(clippy::disallowed_methods, reason = "...")]
        = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.94.0/index.html#disallowed_methods
        = note: requested on the command line with `-D clippy::disallowed-methods`
    
    error: could not compile `fabro-server` (lib) due to 1 previous error
    ```
  - Stderr: (empty)

## Context
- failure_class: transient_infra
- failure_signature: preflight_lint|transient_infra|script failed with exit code: <n> ## stdout error: use of a disallowed method `std::process::command::new` --> lib/crates/fabro-server/src/serve.rs:<n>:<n> | <n> | let child = std::process::command::new("bun") | ^^^^^^^^^^^^^^^^^^^^^^^^^^ |


The preflight lint step failed. Read the build output from context and fix all clippy lint warnings.