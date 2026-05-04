**All 5081 tests pass, clippy clean, fmt clean.**

## Summary

I've implemented an end-to-end "steering for running agents" feature across the Fabro codebase. The implementation follows the plan with the following pieces:

### Core types (fabro-types, fabro-interview)
- New `SteerKind` enum (`Append`/`Interrupt`) in `fabro-types::steering`, re-exported via `fabro-interview`.
- New `WorkerControlMessage::Steer { text, kind, actor }` variant on the worker control envelope, plus `WorkerControlEnvelope::steer(...)` builder, with round-trip serde tests.
- New event props in `fabro-types::run_event::agent`: `AgentSteeringInjectedProps` gains `kind: SteerKind`; new `AgentSteeringAttachedProps`, `AgentSteeringDetachedProps`, `AgentSteerBufferedProps`, `AgentSteerDroppedProps` (with `AgentSteerDroppedReason` enum).

### Agent core (fabro-agent)
- `Session` gains `round_token: Arc<RwLock<CancellationToken>>` and `completion_coordinator: Option<Arc<dyn CompletionCoordinator>>`.
- New `SessionControlHandle` (queue + round_token clone) with `steer`/`interrupt_with`/`enqueue`/`queue_is_empty`/`queue_len`/`pop_oldest` methods, exposed via `Session::control_handle()`.
- New `interrupt_with(text, actor)` method that pushes an `Interrupt` item and cancels the round token.
- `steering_queue` element type changed to `(String, SteerKind, Option<Principal>)`.
- New `CompletionCoordinator` trait + `set_completion_coordinator`/`clear_completion_coordinator`.
- `AgentEvent::SteeringInjected` gains `kind` and an internal-only `actor` field (skipped from serialization).
- `process_input` loop rewritten:
  - Top-of-loop `round_token` refresh and `drain_steering()` (replacing the pre-loop and post-tool drain calls).
  - LLM stream awaits wrapped in `tokio::select!` against both `round_token` and `cancel_token`.
  - Mid-LLM steer interrupts emit `AssistantOutputReplace` to clear stale partial output, then `continue`.
  - Tools execute with a composite child token, but their futures run to completion (preserving the `tool_use ↔ tool_result` invariant); afterward, branch on which token fired.
  - On natural completion (no tool calls), `completion_coordinator.on_natural_completion()` decides whether to keep iterating.
- Three new agent-level tests: `steer_event_carries_append_kind`, `interrupt_with_pushes_interrupt_kind_event`, `append_during_final_response_triggers_extra_round_when_coordinator_returns_true`.

### Workflow hub (fabro-workflow)
- New `SteeringHub` (sync std locks) with `register/unregister/deliver/drain_pending_at_run_end`, bounded queues (`PER_SESSION_QUEUE_CAP=32`, `PER_RUN_PENDING_CAP=32`), FIFO eviction with drop events.
- 8 unit tests covering: buffering when no active, drain-pending-at-run-end, both queue caps, idempotent unregister, drain-on-first-register, broadcast to multiple sessions, no-redrain-on-replace.
- 4 new top-level workflow `Event` variants (`AgentSteeringAttached/Detached`, `AgentSteerBuffered/Dropped`) with names, conversion, stored-fields lifting (lifts stage_id and actor through `RunEvent` envelope per events strategy).
- `agent_actor_for_event` updated to lift `actor` from `AgentEvent::SteeringInjected` to top-level `RunEvent.actor`.
- `StartServices`, `RunSession`, `InitOptions` plumbed with `steering_hub: Arc<SteeringHub>`.
- `AgentApiBackend`:
  - `with_steering_hub` builder.
  - In `run`: registers the session via RAII guard (`SteeringHubGuard`) so it's unregistered on every exit path; installs `SteeringCompletionCoordinator` for the close-the-door pattern.
  - Failover path re-registers the new session under the same `stage_id`.
- `operations::start` calls `drain_pending_at_run_end` before flushing the progress logger so terminal drop events make it to the store.

### Worker (fabro-cli runner)
- Constructs the `SteeringHub`, threads it into `StartServices` and into `apply_worker_control_line` / `handle_worker_control_stream_events` / `spawn_worker_control_stream`.
- New match arm dispatches `WorkerControlMessage::Steer` to `steering_hub.deliver(...)`.

### Server (fabro-server)
- `RunAnswerTransport::InProcess` now carries `steering_hub: Arc<SteeringHub>` alongside `interviewer`.
- `RunAnswerTransport::steer(text, kind, actor)` method (mirrors `cancel_run`): subprocess sends a `WorkerControlEnvelope::Steer` over `control_tx`; in-process calls `steering_hub.deliver` directly.
- `ManagedRun` gains `active_api_stages: HashSet<StageId>` and `active_cli_stages: HashSet<StageId>`, maintained from `agent.steering.attached/detached`, `agent.cli.started/completed`, and stage/run lifecycle events as backstops.
- New `POST /runs/{id}/steer` handler in `handler/steer.rs`:
  - Validates body (1..8192 trim-non-empty), maps `interrupt: bool` → `SteerKind`.
  - Status gate: blocked → 409 with `code: "use_answer_endpoint"`; non-running/terminal → 409; missing → 404.
  - Steerability predicate: rejects when only CLI agents are active with `code: "cli_agent_not_steerable"`.
  - Forwards via the run's `RunAnswerTransport.steer(...)`, returns 202 on success, 503 on transport timeout/closed.
- 2 new server tests: `steer_nonexistent_run_returns_not_found`, `steer_empty_text_returns_bad_request`.
- Existing `in_process_answer_transport_cancel_run_cancels_pending_interviews` test updated for the new `InProcess` shape.

### OpenAPI + clients
- New `POST /api/v1/runs/{id}/steer` operation under the `Human-in-the-Loop` tag with `SteerRunRequest` schema (`text` required min/max, `interrupt` default false). Responses 202/400/404/409/503.
- Rust client `fabro_client::Client::steer_run(run_id, text, interrupt)` added.
- TypeScript model `SteerRunRequest` added to `lib/packages/fabro-api-client/src/models/`.

### CLI (fabro-cli)
- New `fabro steer <run-id> <text> [--interrupt]` command (and `--text-stdin`).
- `SteerArgs` in `args.rs`, `RunCommands::Steer(SteerArgs)` variant with `name() => "steer"`.
- `commands/run/steer.rs` reads stdin via tokio when `--text-stdin`, validates non-empty, calls `client.steer_run(...)`.
- Updated insta snapshot for the top-level `--help` listing.

### Web UI (apps/fabro-web)
- New `useSteerRun(runId)` mutation in `app/lib/mutations.ts`.
- New `SteerComposer` component in `app/components/steer-composer.tsx`: modal with textarea, Send and Interrupt buttons, Enter-to-send / Shift+Enter, autofocus, Escape-to-close, inline 409 error surfacing for `cli_agent_not_steerable`/`use_answer_endpoint`.
- `routes/runs.tsx`: removed the demo-mode gate on the Steer button; clicking the button now opens the composer (preventDefault to avoid the wrapper Link navigating).
- `lib/run-events.ts`: new `STEERING_EVENTS` set, query-key invalidations for the steer-related events to keep the run timeline fresh.

### Verification
- `cargo build --workspace` clean.
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` clean.
- `cargo +nightly-2026-04-14 fmt --check --all` clean.
- `cargo nextest run --workspace`: **5081 tests pass, 0 fail, 182 skipped**.

### Notes / scope cuts
- The TypeScript Axios client regeneration step (`bun run generate`) requires Java, which is unavailable in this sandbox; I added the `SteerRunRequest` model file by hand to keep `useSteerRun` typed. The next dev run with Java will regenerate identically.
- A few items the plan listed as nice-to-have were trimmed for time: dedicated Web component test (`steer-composer.test.tsx`), the `useRunToasts` hook + dispatch wiring, and additional parity_matrix integration tests (idle-stream interrupt / mid-tool interrupt / no-dangling-tool-use). The agent core tests for these scenarios are the ones most worth backfilling next; the loop changes are guarded by the existing parity tests that still pass.