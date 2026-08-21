# Fabro Main Event Stream Design

## Context

Fabro currently has durable workflow run event streams. Each run stores canonical
`RunEvent` records under a run-scoped SlateDB prefix:

```text
runs/<run_id>/events/<seq-ts>
```

Those per-run events drive run projections, run event APIs, attach/SSE, CLI
progress, and live server broadcasts. They are also deleted when the run is
deleted.

The server also has an in-memory `global_event_tx`, but that is only a live
broadcast of per-run events for active server consumers. It is not durable and
is not the "main event stream" needed for future automation triggers.

This design creates the first chunk of the main event stream: a durable,
server-wide event log for coarse internal Fabro lifecycle events. It does not
include GitHub webhook ingestion, automation trigger matching, public APIs, or
listener cursor persistence.

## Goals

- Add a durable main event stream in the existing SlateDB database.
- Store main events under a global prefix that survives run deletion.
- Mirror selected coarse run lifecycle events into the main stream.
- Give main events their own identity, sequence, and timestamp.
- Preserve enough compact event details for future automation matching and audit
  even after a run is deleted.
- Keep storage generic and place lifecycle mirroring policy in `fabro-server`.

## Non-Goals

- Do not ingest GitHub webhook deliveries in this chunk.
- Do not add event-triggered automations or matcher syntax.
- Do not add public API, SSE, or CLI surfaces for the main stream.
- Do not persist listener cursors or retry state.
- Do not mirror stage, agent, tool, transcript, sandbox, or detailed run events.
- Do not duplicate large run payloads such as full graphs, settings, patches,
  transcripts, or tool output.

## Main Event Model

Add a new shared type module in `fabro-types` named `main_event`.

`MainEventEnvelope` contains:

- `seq`: monotonically increasing main-stream sequence number.
- `event`: the `MainEvent`.

`MainEvent` contains:

- `id`: new UUIDv7-style event id for the main stream record.
- `ts`: append timestamp for the main stream record.
- `event`: the main event name through a typed `MainEventBody`.
- `properties`: typed body data through serde's tagged enum shape.

Each mirrored run lifecycle event gets a distinct main event name:

- `fabro.run.created`
- `fabro.run.started`
- `fabro.run.running`
- `fabro.run.completed`
- `fabro.run.failed`
- `fabro.run.cancelled`
- `fabro.run.paused`
- `fabro.run.unpaused`

`run.cancel.requested` is intentionally not mirrored in v1. Terminal
cancellation is represented today by `run.failed` with cancellation semantics,
so the main stream emits `fabro.run.cancelled` for that terminal condition.

All mirrored events include source metadata:

- `run_id`
- `source_event_id`
- `source_event_ts`
- `source_event_name`
- `actor`, when present

Lifecycle bodies include compact standalone summaries:

- Created: title, workflow slug, automation ref, git context, parent id, and
  provenance subject or actor information.
- Started/running: run id, source metadata, and lightweight start metadata when
  present.
- Completed: success reason/status, final commit SHA, compact billing summary,
  automation ref when available, and source metadata.
- Failed: failure reason/category/message summary, final commit SHA, compact
  billing summary, automation ref when available, and source metadata.
- Cancelled: same source shape as failed, but named as a cancellation event and
  containing cancellation reason/message summary.
- Paused/unpaused: run id and source metadata.

The main event stream must stand on its own after run deletion, but it should
not become a second full run event log.

## Storage

Add a main event store to `fabro-store::Database` using the same SlateDB
database as runs. Events are stored under:

```text
events/main/<seq-ts>
```

The store supports internal operations:

- append a `MainEventBody` and return a `MainEventEnvelope`.
- list events from `since_seq` with a bounded limit.
- recover the next sequence number on open by scanning the prefix.
- subscribe to live appended main events for future internal consumers.

Run deletion remains scoped to `runs/<run_id>/...`, so it does not remove main
events.

`fabro-store` should not decide which run events are worth mirroring. It only
stores and replays main events.

## Server Mirroring

`fabro-server` owns the mirroring policy.

The first implementation point is the existing active-run forwarding path that
subscribes to each active run's event stream and forwards events into
`global_event_tx`. When a run event arrives:

1. Reconcile the server's live run state as it does today.
2. Attempt to map the `RunEvent` to a `MainEventBody`.
3. Append the main event if the run event is one of the selected lifecycle
   events.
4. Ignore all non-v1 run events.
5. Continue broadcasting the run event through the existing in-memory bus.

This keeps product policy in the server and leaves `fabro-store` as the durable
storage layer.

## Error Handling

This chunk follows the current practical behavior of run event persistence:

- Server/API lifecycle append paths already fail when their run event append
  fails.
- Workflow runtime event logging can be warning-only through the async run event
  logger.

Main event mirroring from the active-run forwarding path logs a warning on append
failure and does not stop or fail the running workflow. This is acceptable for
the first chunk because there is no automation listener yet. A later automation
trigger chunk can introduce stricter fail-closed wrappers for critical server
transitions if trigger correctness requires them.

## Testing

Add focused coverage at three layers.

`fabro-types`:

- `MainEvent` serde round trips for each v1 event name.
- Event name mapping is explicit and stable.
- Unknown event names deserialize into an `Unknown` body that preserves the
  original name and raw properties, matching the compatibility style of
  `RunEvent`.

`fabro-store`:

- Append and list main events under `events/main/<seq-ts>`.
- Sequence numbers are stable and recover after reopening the database.
- `delete_run` does not delete main events.
- Bounded list-from behavior returns ordered events from `since_seq`.

`fabro-server`:

- Active run lifecycle events mirror into the main event store.
- Non-v1 events such as stage or agent events are ignored.
- Terminal cancelled `run.failed` maps to `fabro.run.cancelled`.
- Normal `run.failed` maps to `fabro.run.failed`.
- Mirroring failures are logged and do not stop forwarding the original run
  event.

## Deferred Follow-Up Work

- GitHub webhook producer: convert verified webhook deliveries into durable main
  events with raw provider payloads and idempotency metadata.
- Automation event triggers: add deterministic matcher syntax and an internal
  replay-plus-tail listener over the main event store.
- Listener cursors: persist per-listener processing state and retry behavior.
- Public read/stream APIs: expose main events to operators or external
  consumers only after the internal model has settled.
- Stronger failure semantics: add server wrappers for critical lifecycle paths
  if automation correctness requires fail-closed mirroring.
