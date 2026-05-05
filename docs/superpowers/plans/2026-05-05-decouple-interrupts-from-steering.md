# Decouple Interrupts From Steering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split run interruption from steering message delivery while keeping `steer interrupt=true` as ergonomic sugar.

**Architecture:** `run.interrupt` becomes a standalone control/event path that cancels the active API-mode agent round and waits for later steering. `run.steer` becomes a plain message-injection path with no delivery kind. The existing combined user flow applies interrupt first, then steer, preserving user convenience without coupling the concepts in queue/event types.

**Tech Stack:** Rust workspace crates (`fabro-agent`, `fabro-workflow`, `fabro-server`, `fabro-interview`, `fabro-api`, `fabro-client`), OpenAPI, generated TypeScript API client, React web app, SSE run events.

---

## Summary

Split steering message delivery from agent interruption. `run.steer` becomes a plain "user injected guidance" event with no `kind`; `run.interrupt` becomes a separate control/event path that cancels the active agent round. `POST /runs/{id}/steer { interrupt: true }` remains ergonomic sugar for "interrupt, then steer," but there is no standalone CLI interrupt command.

Standalone interrupts cancel the current active API-mode agent round, keep the stage's steering lease active, and wait at a steerable point until a later steer resumes the agent.

## Key Changes

- Public API:
  - Add `POST /api/v1/runs/{id}/interrupt` with `202`, `404`, `409`, and `503` behavior matching steer/cancel conventions.
  - Keep `POST /api/v1/runs/{id}/steer`; keep `interrupt?: boolean` as request convenience.
  - For `steer interrupt=true`, require an active API-mode agent session and send one atomic worker-control operation that applies interrupt first, then enqueues the steer.
  - Non-interrupt steer may still buffer when no API session is active and no CLI agent is running.
  - Regenerate Rust and TypeScript API clients after OpenAPI updates.

- Control protocol and types:
  - Replace `WorkerControlMessage::Steer { text, kind, actor }` with `Steer { text, actor }`.
  - Add `WorkerControlMessage::Interrupt { actor }`.
  - Add `WorkerControlMessage::InterruptThenSteer { text, actor }` for the combined convenience path. The server must send this as one control envelope; do not implement `steer interrupt=true` as two independent enqueue operations.
  - Remove `SteerKind` from `fabro-types`, `fabro-agent`, workflow event payloads, and web client assumptions.
  - Keep existing CLI `fabro steer --interrupt`; implement it through the existing steer API, not a new CLI command.

- Workflow/session behavior:
  - Split `SessionControlHandle` into plain `steer(text, actor)` and plain `interrupt(actor)`.
  - Add an explicit agent-side `waiting_for_steer` state, guarded by the same control state as the steering queue.
  - Steering enqueues text, clears `waiting_for_steer`, and wakes the session.
  - Interrupt cancels the current round token and, when the steering queue is empty, sets `waiting_for_steer` so the session cannot immediately start another LLM round with unchanged context.
  - Duplicate pure interrupts while already `waiting_for_steer` are idempotent: accept them, emit another `run.interrupt`, keep the session waiting, and do not enqueue synthetic steering.
  - If an interrupt cancels an LLM stream or tool round without queued steering, the session waits without closing/deactivating its steering lease; terminal run cancellation still wins immediately.
  - If interrupt and steer are applied together, the steer resumes the waiting/next round immediately.
  - Terminal run cancellation must wake/break any `waiting_for_steer` wait and return the existing cancellation error path.
  - Update the natural-completion close-the-door path so the activation lease is kept alive when the steering queue is nonempty or `waiting_for_steer` is true. `CompletionCoordinator`, `ActivationLease::release_if_queue_empty`, and `SessionControlHandle` should expose/use a single "has pending control work" predicate instead of checking queue emptiness alone.

- Events:
  - Add top-level persisted `run.interrupt` with `actor` in the envelope and empty properties.
  - Add top-level persisted `run.steer` with `actor` in the envelope and `properties.text`.
  - Keep `agent.steering.injected`, but remove `kind`; it means the steer actually entered agent history.
  - Keep `agent.steer.buffered` and `agent.steer.dropped`, but remove `kind` from buffered.
  - Update run-event conversion, stored fields, docs, SSE invalidation, and toast logic for the simplified payloads.

- Event ownership and ordering:
  - The worker-side control handler / `SteeringHub` is the single source of truth for persisted `run.interrupt` and `run.steer`; API handlers only emit these events indirectly after the live worker accepts the control envelope.
  - A failed or timed-out control-channel request must not emit `run.interrupt` or `run.steer`.
  - For `InterruptThenSteer`, persisted order must be `run.interrupt`, then `run.steer`, then later `agent.steering.injected` only when the text is drained into agent history.

## Protocol vs Events

Worker-control messages are transport commands, not persisted `RunEvent` names. `run.interrupt_then_steer` exists only as a worker-control envelope for atomic delivery of the combined convenience path and must never be emitted as a persisted run event. The only persisted run-level event names introduced here are `run.interrupt` and `run.steer`.

## API Response Matrix

| Run state | `POST /steer` | `POST /steer { interrupt: true }` | `POST /interrupt` |
| --- | --- | --- | --- |
| Active API-mode session | `202` accepted; emits `run.steer`; later `agent.steering.injected` | `202` accepted atomically; emits `run.interrupt`, then `run.steer` | `202` accepted; emits `run.interrupt` |
| Active API-mode session already `waiting_for_steer` | `202` accepted; emits `run.steer`; clears wait and later emits `agent.steering.injected` | `202` accepted atomically; emits `run.interrupt`, then `run.steer`; clears wait | `202` accepted idempotently; emits `run.interrupt`; remains waiting |
| No active API session, no active CLI agent | `202` accepted; emits `run.steer`, then `agent.steer.buffered` | `409` `no_active_api_session` | `409` `no_active_api_session` |
| Active CLI-only agent stages | `409` `cli_agent_not_steerable` | `409` `cli_agent_not_steerable` | `409` `cli_agent_not_steerable` |
| Blocked on interview/question | `409` `use_answer_endpoint` | `409` `use_answer_endpoint` | `409` `use_answer_endpoint` |
| Terminal run | `409` `run_not_steerable` | `409` `run_not_steerable` | `409` `run_not_interruptible` |
| Missing live worker channel | `503` `worker_control_unavailable` | `503` `worker_control_unavailable` | `503` `worker_control_unavailable` |
| Archived run | Existing archived-run rejection response from `reject_if_archived` | Existing archived-run rejection response from `reject_if_archived` | Existing archived-run rejection response from `reject_if_archived` |

## Implementation Checklist

- [x] OpenAPI/API clients: add `/runs/{id}/interrupt`, keep `SteerRunRequest.interrupt`, regenerate `fabro-api`, `fabro-client` usage, and TypeScript API client models.
- [x] Server route and transport: add interrupt handler, add `RunAnswerTransport::interrupt`, add `RunAnswerTransport::interrupt_then_steer`, enforce the API response matrix, and ensure combined steer uses one control operation.
- [x] Worker protocol: simplify `run.steer`, add `run.interrupt`, add `run.interrupt_then_steer`, and update subprocess/in-process dispatch.
- [x] Agent session state: replace `SteerKind` queue items with plain text+actor, add `waiting_for_steer`, wake on steer, block after pure interrupt, make duplicate interrupts idempotent while waiting, and break wait on terminal cancellation.
- [x] Natural-completion lease safety: update `CompletionCoordinator`, `ActivationLease::release_if_queue_empty`, and `SessionControlHandle` so close-the-door keeps the lease alive when either the queue is nonempty or `waiting_for_steer` is true.
- [x] Steering hub behavior: expose plain `deliver_steer`, `interrupt`, and `interrupt_then_steer`; emit accepted control events in the required order; keep buffering/dropping only for steering text.
- [x] Events/schema/store/web: add `run.interrupt` and `run.steer`, simplify `agent.steering.injected` and `agent.steer.buffered`, update stored fields, docs, run-state projections, SSE invalidation, and toasts.
- [x] Docs: update internal event docs, public API reference, CLI docs for `fabro steer --interrupt`, and steering docs to explain interrupt as separate control flow.
- [x] Verification: run the Rust and web test commands listed below, plus formatting and clippy.

## Test Plan

- `fabro-interview` worker-control envelope tests: JSON round-trip tests for transport-only `run.interrupt`, simplified `run.steer`, and transport-only `run.interrupt_then_steer` preserving `text` and `actor`.
- `fabro-types` / `fabro-api` persisted event tests: `run.interrupt` and `run.steer` serialize as persisted `RunEvent`s; no persisted event named `run.interrupt_then_steer` exists.
- `fabro-agent`: unit tests for plain steer injection, LLM-stream interrupt entering `waiting_for_steer`, tool-round interrupt entering `waiting_for_steer`, pure interrupt racing with a no-tool natural completion without releasing the lease, later steer wake-up, duplicate interrupt while waiting, interrupt-plus-steer resuming without an extra wait, and terminal cancel breaking the wait.
- `fabro-workflow`: steering hub tests for buffering plain steers, dropping plain steers, broadcasting interrupts to active sessions, and preserving interrupt-before-steer ordering.
- `fabro-server`: handler tests for `/interrupt`, `steer interrupt=true`, the full API response matrix, missing worker channel, CLI-only conflict, terminal/blocked conflicts, and stale activation/deactivation behavior.
- `fabro-cli` runner/subprocess bridge: worker-control line handler tests for simplified `run.steer`, `run.interrupt`, and `run.interrupt_then_steer`.
- `fabro-store` and run-event tests: projection ignores top-level `run.interrupt`/`run.steer` except where explicitly tracked; `agent.session.activated` remains the provider-used source.
- Web tests: update run-event invalidation and toast assertions for `run.interrupt`, `run.steer`, simplified `agent.steering.injected`, and simplified `agent.steer.buffered`.
- Verification commands:
  - `cargo build -p fabro-api`
  - `cd lib/packages/fabro-api-client && bun run generate`
  - `cargo nextest run --workspace`
  - `cd apps/fabro-web && bun test && bun run typecheck`
  - `cargo +nightly-2026-04-14 fmt --check --all`
  - `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings`

## Assumptions

- Event names are exactly `run.interrupt` and `run.steer`.
- Standalone interrupt is API-only for now; no `fabro interrupt` CLI command and no new standalone web button are required.
- A pure interrupt is not a failure, pause, or cancellation of the run; it is a mid-stage wait point that resumes only when steering arrives or the run is terminally cancelled.
- Non-interrupt steering keeps the current buffering behavior, but interrupt steering does not buffer the interrupt portion when no active API session exists.
- Duplicate pure interrupts while already waiting are idempotent `202` responses that emit another persisted `run.interrupt` and leave the wait state unchanged.
