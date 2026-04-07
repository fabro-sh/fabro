---
title: "feat: unify interview handling around projected questions, worker stdin, and server-owned Slack"
type: feat
status: active
date: 2026-04-07
---

# feat: unify interview handling around projected questions, worker stdin, and server-owned Slack

## Overview

Replace the current split interview architecture with one canonical model:

- pending interviews live in the durable run projection
- answers enter through the server
- active runs receive accepted answers through a live control sink
- Slack becomes a server-owned delivery surface, not a special in-memory interviewer path

This refactor removes `FileInterviewer` and `WebInterviewer`, keeps the existing HTTP question and answer routes as the canonical external contract, and uses worker `stdin` JSONL for server-to-worker answer delivery.

## Problem Frame

Interview handling is currently split across two transport-specific implementations:

- subprocess runs use `FileInterviewer`, with `interview_request.json` and `interview_response.json` scratch files
- the in-process server override path uses `WebInterviewer`, with pending questions and answer waiters held only in memory

That split leaks into the server:

- `GET /runs/{id}/questions` branches between `WebInterviewer.pending_questions()` and the request scratch file
- `POST /runs/{id}/questions/{qid}/answer` branches between `WebInterviewer.submit_answer()` and the response scratch file
- Slack is built on the same special `WebInterviewer` path instead of the canonical server question API

The result is one feature implemented twice, with two incompatible sources of truth for pending questions and two different answer delivery mechanisms.

The desired architecture is simpler:

- the durable run event stream and run projection are authoritative for pending interviews
- the server owns answer validation and first-answer-wins behavior
- subprocess workers receive accepted answers over `stdin`, which also establishes the future control plane for steering
- Slack is just another server-owned client of the canonical answer path

## Scope Boundaries

In scope:

- add stable question ids and persist pending interviews in the run projection
- replace scratch-file answer transport with worker `stdin` JSONL
- replace `WebInterviewer` with the same brokered answer model used by subprocess runs
- move Slack onto a single global Socket Mode listener inside `fabro-server`
- keep the existing HTTP question and answer routes as the canonical external API
- expand the question API payload to expose the information needed by CLI and Slack

Out of scope:

- worker-side steering behavior beyond reserving the `stdin` protocol shape for it
- server restart rehydration of Slack delivery state
- a separate Slack bridge process
- signed Slack tokens or tamper-evident action payloads
- any new public answer route or worker-side HTTP contract

## Requirements Trace

- R1. Pending interview state must be derived from durable run events and exposed from the run projection, not from scratch files or live interviewer objects.
- R2. Every interview question must have a stable `question_id` that survives through events, HTTP APIs, Slack payloads, and worker answer delivery.
- R3. `GET /runs/{id}/questions` must read only from canonical projected pending interview state.
- R4. `POST /runs/{id}/questions/{qid}/answer` must remain the canonical answer ingress for web, CLI, and Slack.
- R5. For subprocess runs, the server must deliver accepted answers to the worker over `stdin` as versioned JSONL.
- R6. For the in-process `registry_factory_override` path, the server must still support interview-driven tests without `WebInterviewer`.
- R7. `FileInterviewer` and `WebInterviewer` must be removed from production use and then deleted.
- R8. Slack must be owned by `fabro-server` as a single global Socket Mode listener, not by per-run workers and not by a separate bridge process.
- R9. Slack delivery state may be memory-only. After server restart, stale Slack prompts may be ignored and resumed runs may post fresh prompts under the new server config.
- R10. Slack payloads must carry `run_id` and `qid`, and Slack answers must route through the same canonical server answer handler as HTTP.
- R11. Multiple-choice and multi-select answers must remain structured end-to-end and must not be flattened to text in the Slack path.
- R12. Accepted answers must use first-answer-wins semantics across concurrent HTTP and Slack submissions.

## Key Decisions

- `Question` gains a first-class `id: String`.
  - Generate it in the human handler as a ULID string.
  - Do not hide it in `metadata`.
  - Do not add a new `QuestionId` wrapper type in this pass.

- `interview.started` becomes the authoritative pending-question event.
  - Keep `question` as the human-readable text field.
  - Add `question_id`, `stage`, `question_type`, `options`, `allow_freeform`, `timeout_seconds`, and `context_display`.

- `interview.completed` and `interview.timeout` gain `question_id`.

- Add `interview.aborted`.
  - Carry `question_id`, `question`, `stage`, `reason`, and `duration_ms`.
  - Use it when the interviewer returns `Aborted` or `Skipped`, so pending interview cleanup stays event-driven.

- `RunProjection` becomes the only source of truth for pending interviews.
  - Add `pending_interviews: BTreeMap<String, PendingInterviewRecord>`.
  - Insert on `interview.started`.
  - Remove on `interview.completed`, `interview.timeout`, `interview.aborted`, `run.rewound`, and terminal run events.

- Keep the existing answer route contract.
  - `GET /runs/{id}/questions` and `POST /runs/{id}/questions/{qid}/answer` remain the public surface.
  - `SubmitAnswerRequest` already supports freeform, single-select, and multi-select and does not need to change.
  - Expand `ApiQuestion` with `stage`, `timeout_seconds`, and `context_display`.

- Server-to-worker answer delivery uses `stdin` JSONL, versioned from day one.
  - Use one JSON object per line.
  - Implement only `interview.answer` in this pass.
  - Reserve the envelope for future steering, but do not implement steering behavior yet.

- Use a broker-backed interviewer abstraction for both runtime paths.
  - Replace `FileInterviewer` and `WebInterviewer` with an internal `InterviewBroker` plus `ControlInterviewer`.
  - For subprocess runs, the broker is fed by parsed `stdin` control messages.
  - For the in-process override path, the broker is fed directly by the server’s canonical answer submission service.

- Slack is a server-owned delivery surface, not a worker concern.
  - Start one Socket Mode listener inside `fabro-server` when both Slack tokens and `slack.default_channel` are configured.
  - Have that service consume the server's existing global run-event broadcast, the same fanout fed by `forward_run_events_to_global(...)` for both subprocess and in-process runs.
  - Do not read from SSE endpoints or introduce a second event source.
  - Keep Slack message metadata and thread routing in server memory only.
  - Do not replay or restore Slack posts after restart.

- Slack action payloads use plain JSON in `value`.
  - Carry at least `run_id`, `qid`, and answer metadata.
  - Keep `action_id` structural rather than encoding identifiers into it.
  - Validate all incoming Slack answers against the current projected pending interview before accepting them.

- `ControlInterviewer` is the runtime `Interviewer` implementation.
  - `inform()` remains a no-op.
  - `ask_multiple()` continues to use the trait default, so multiple concurrent `ask()` calls must work correctly when keyed by `qid`.

- `InterviewBroker` owns only live waiter state.
  - It does not own canonical pending-question state; `RunProjection.pending_interviews` does.
  - Its server/transport injection surface is `submit(qid, answer) -> Result<(), SubmitError>`.
  - Unknown, already-resolved, or duplicate `qid`s are rejected by the broker.

- Event replay must stay backward compatible.
  - New interview event fields must deserialize with defaults so old `progress.jsonl` entries still replay.
  - Historical `interview.started` events that lack `question_id` must not populate `pending_interviews`.
  - Historical runs continue to replay normally, but projection-backed pending interview reconstruction is only guaranteed for runs created after this change.

- The old file-claim reattach window is intentionally removed.
  - Client detach and reattach no longer matter because the pending question is durable in the projection and can be fetched again later.
  - Only loss of the live server-worker control channel is treated as fatal for the waiting interview.

- First-answer-wins is enforced in memory before transport delivery.
  - Projection lookup proves the question is still pending.
  - A per-run acceptance guard must then claim `qid` exactly once before any answer is sent to the worker.
  - If transport delivery fails, that claim is released so a later submission can retry.
  - If transport delivery succeeds, the claim stays until the interview lifecycle event clears the pending question.

## Recommended Delivery Order

Recommended sequencing:

- Phase A: steps 1 and 2 together
  - lock the question id, event shape, replay compatibility, projection model, and API question shape first

- Phase B: steps 3, 4, and 5 together
  - implement `InterviewBroker`, `ControlInterviewer`, canonical answer submission, subprocess `stdin` transport, in-process override migration, and CLI attach adjustments in one pass
  - steps 3 and 4 should land together because they both restructure the live answer transport in `server.rs`

- Phase C: step 6
  - migrate Slack after the canonical pending-question model and answer submission path are stable

- Phase D: step 7
  - remove dead implementations, examples, and outdated docs only after the new path is covered by tests

Parallelizable work after Phase A is in place:

- worker-side `ControlInterviewer` and JSONL parsing
- CLI `ApiQuestion` conversion updates
- Slack payload and parsing refactor

## Implementation Changes

### 1. Canonical question ids and interview events

Update the workflow and event model so the pending interview can be reconstructed without a live transport object.

- Modify `lib/crates/fabro-interview/src/lib.rs`:
  - add `Question.id`
  - keep `metadata` and `default` as internal-only fields
  - export the new broker-backed interviewer implementation

- Modify `lib/crates/fabro-workflow/src/handler/human.rs`:
  - generate `question.id`
  - emit the richer `interview.started`
  - emit `interview.aborted` for `Aborted` and `Skipped`
  - emit `question_id` on `interview.completed` and `interview.timeout`

- Modify `lib/crates/fabro-workflow/src/event.rs` and `lib/crates/fabro-types/src/run_event/misc.rs`:
  - extend the interview event variants and payload props to carry the full pending-question surface
  - keep event names stable except for adding `interview.aborted`

### 2. Run projection and API question surface

Update the store projection and public question API to surface the canonical pending interview state.

- Modify `lib/crates/fabro-store/src/run_state.rs`:
  - add `pending_interviews`
  - add `PendingInterviewRecord`
  - apply insert and cleanup rules for all interview lifecycle events

- Modify `docs/api-reference/fabro-api.yaml`:
  - expand `ApiQuestion` with `stage`, `timeout_seconds`, and `context_display`
  - leave `SubmitAnswerRequest` unchanged

- Regenerate generated API clients:
  - Rust via `cargo build -p fabro-api`
  - TypeScript via `cd lib/packages/fabro-api-client && bun run generate`

### 3. Replace file transport with worker `stdin` JSONL

Replace scratch-file answer delivery with a live control channel into the worker.

- Add `InterviewBroker` and `ControlInterviewer` in `lib/crates/fabro-interview/src/lib.rs` and supporting modules:
  - `ControlInterviewer` implements `Interviewer`
  - `ControlInterviewer::ask(question)` registers a oneshot waiter under `question.id` and awaits broker delivery
  - `InterviewBroker` owns only live waiter state, such as `HashMap<qid, oneshot::Sender<Answer>>`
  - `InterviewBroker::submit(qid, answer)` resolves a waiter exactly once and returns a typed error for unknown or already-resolved questions
  - `inform()` remains a no-op
  - `ask_multiple()` continues to use the trait default and is supported by allowing concurrent per-`qid` waiters

- Modify `lib/crates/fabro-server/src/server.rs`:
  - spawn workers with `stdin(Stdio::piped())`
  - store a per-run live answer transport instead of a `WebInterviewer`
  - use a bounded per-run `mpsc::Sender<WorkerControl>` feeding a dedicated JSONL pump into worker stdin
  - enqueue control messages with a short timeout; if enqueue times out or the channel is closed, clear any acceptance claim and return a transient server error rather than hanging the request
  - run a JSONL pump that writes accepted answers into the worker stdin pipe
  - replace the current `get_questions` and `submit_answer` branches with one projection-backed question query plus one internal answer-submission service
  - implement canonical answer submission in this order:
    - load the pending question from the projection
    - validate and build the `Answer`
    - acquire the per-run acceptance guard and claim `qid` exactly once
    - submit the answer to the live transport
    - release the claim on transport failure
    - keep the claim until interview completion, timeout, or abort clears the pending question
  - reject a second concurrent submission with `409` once the acceptance guard says the question was already claimed

- Modify `lib/crates/fabro-cli/src/commands/run/runner.rs`:
  - replace `FileInterviewer` with `ControlInterviewer`
  - start a `stdin` reader task that parses versioned JSONL `WorkerControl` messages
  - resolve broker waiters by `qid`
  - treat unexpected control-channel closure as interviewer abort/failure rather than hanging forever
  - keep a dedicated stdin reader task running for the life of the worker so answer messages do not block behind unrelated stage execution

- Define a wire-specific answer payload.
  - Do not serialize internal `Answer` directly on the wire.
  - Use an explicit `kind` shape such as `yes`, `no`, `text`, `selected`, and `multi_selected`.

- Remove scratch-file interview transport.
  - delete the request and response file helpers from the server
  - stop creating or reading `interview_request.json` and `interview_response.json` for server-managed runs
  - intentionally remove the old claim-file reattach window because pending questions are now projection-backed and re-fetchable after client reconnect

### 4. Replace `WebInterviewer` in the in-process override path

Keep the test override path, but move it onto the same architecture as subprocess runs.

- Modify `lib/crates/fabro-server/src/server.rs`:
  - replace `ManagedRun.interviewer` with a live answer transport enum such as:
    - subprocess `stdin` control sender
    - in-process broker handle
  - keep `create_app_state_with_registry_factory(...)` and `execute_run_in_process(...)`
  - pass a broker-backed `Arc<dyn Interviewer>` into the override registry instead of `WebInterviewer`

- Keep the external behavior unchanged for tests.
  - `GET /runs/{id}/questions` still lists pending questions
  - `POST /runs/{id}/questions/{qid}/answer` still satisfies waiting in-process interview gates
  - the difference is that both now go through the projection and canonical submission service

### 5. Keep CLI attach on the canonical server question path

Do not make attach parse the event payload into the prompt directly in this pass.

- Keep `lib/crates/fabro-cli/src/commands/run/attach.rs` event-driven behavior:
  - `interview.started` remains the trigger
  - attach fetches pending questions from the server
  - attach prompts locally with `ConsoleInterviewer`
  - attach submits answers through the existing server client

- Update CLI question conversion for any new `ApiQuestion` fields that should be shown, especially `context_display`.

### 6. Move Slack onto server-owned canonical answer handling

Replace the current `WebInterviewer`-centric Slack path with a server-owned integration.

- Add server-side Slack wiring in `lib/crates/fabro-server/src/server.rs`:
  - create a single Slack service on startup when both tokens and `slack.default_channel` are configured
  - subscribe it to the existing `state.global_event_tx` broadcast and filter `interview.started`, `interview.completed`, `interview.timeout`, and `interview.aborted`
  - post a Slack message for each fresh pending interview
  - use completion, timeout, and abort events for best-effort Slack message updates while the in-memory message metadata still exists

- Refactor `lib/crates/fabro-slack`:
  - remove `WebInterviewer` dependencies from the Socket Mode connection path
  - change parsed interactions to carry `run_id` and `qid`
  - use structural `action_id` values and JSON `value` payloads
  - preserve structured multiple-choice and multi-select answers
  - change freeform thread routing from `thread_ts -> question_id` to `thread_ts -> (run_id, qid)`

- Keep Slack delivery state memory-only inside the server.
  - store `(run_id, qid) -> posted message metadata`
  - store `thread_ts -> (run_id, qid)`
  - after restart, do not rehydrate these maps
  - reject or ignore stale Slack interactions that no longer match a live pending interview

- Route Slack answers through the canonical internal answer-submission service.
  - Slack must not call interviewer objects directly
  - if the answer is accepted, update the original Slack message when metadata is still present

### 7. Remove obsolete interview implementations and docs

After replacement coverage is in place:

- delete `lib/crates/fabro-interview/src/file.rs`
- delete `lib/crates/fabro-interview/src/web.rs`
- remove their re-exports from `lib/crates/fabro-interview/src/lib.rs`
- delete or rewrite `lib/crates/fabro-slack/examples/slack_e2e.rs`
- update `docs/integrations/slack.mdx` so it describes the new server-owned projected-question architecture instead of the old web interviewer model

## Public Interface Changes

- `ApiQuestion` adds:
  - `stage`
  - `timeout_seconds`
  - `context_display`

- `SubmitAnswerRequest` does not change.

- Run events change as follows:
  - `interview.started` carries the full pending-question payload
  - `interview.completed` gains `question_id`
  - `interview.timeout` gains `question_id`
  - `interview.aborted` is new

- Slack interactive payloads change from `question_id`-only routing to explicit `run_id + qid` routing in the action value payload.

## Test Plan

- `fabro-store`
  - add projection tests for pending interview insert and cleanup
  - cover `completed`, `timeout`, `aborted`, rewind, and terminal run cleanup

- `fabro-workflow`
  - add handler tests that verify:
    - generated `question.id`
    - richer `interview.started`
    - `interview.aborted` on `Aborted` and `Skipped`
    - correlated `question_id` on completion and timeout
    - old interview events without the new fields still deserialize and replay safely

- `fabro-interview`
  - add broker tests for:
    - waiter registration by `qid`
    - answer delivery
    - unknown `qid`
    - duplicate answer handling
    - control-channel closure behavior

- `fabro-server`
  - add tests that verify:
    - `GET /runs/{id}/questions` is projection-backed only
    - `POST /runs/{id}/questions/{qid}/answer` works for both subprocess and in-process runs
    - first-answer-wins returns success once and `409` thereafter
    - no interview scratch files are created for server-managed runs
    - `registry_factory_override` still supports interview-driven tests without `WebInterviewer`
    - a full-loop integration test covers:
      - server starts a run with a human gate
      - worker emits `interview.started`
      - pending question appears in `GET /runs/{id}/questions`
      - `POST /runs/{id}/questions/{qid}/answer` succeeds
      - the server delivers the answer over worker `stdin`
      - the workflow continues to terminal completion

- `fabro-slack`
  - add tests that verify:
    - button payloads carry `run_id` and `qid`
    - freeform thread routing uses `(run_id, qid)`
    - multi-select remains structured
    - stale interactions are rejected after the pending interview is gone
    - Slack answers use the same canonical answer-submission service as HTTP

## Assumptions

- `Question.metadata` and `Question.default` remain internal runtime fields and are not exposed in `ApiQuestion`.
- `context_display` is the only contextual prompt payload exposed externally in this pass.
- Slack posts only when both Slack tokens and `slack.default_channel` are configured.
- Slack delivery metadata is intentionally ephemeral and may be lost on server restart.
- `stdin` JSONL is the long-term worker control plane; this pass only implements interview answers, not steering behavior.
