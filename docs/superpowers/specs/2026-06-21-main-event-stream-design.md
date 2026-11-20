# Durable main event stream

Updated 2026-10-16 for upstream's Petri run stream and SQLite storage.

## Purpose

A run's canonical history is Petri's execution records and Fabro's platform
records, delivered together as `RunStreamItem`. This design adds an independent,
compact server-wide lifecycle stream for future cross-run consumers. It does
not replace the run stream, its projection, APIs, attach broadcast, or CLI.

The implementation follows the lifecycle mirror maintained in the ipt fork,
using upstream's current SQLite store in place of the fork's SlateDB store.

## Event contract

`fabro-types::MainEventEnvelope` contains a global `seq` and a `MainEvent`.
The main event has its own UUID v7 `id`, append timestamp `ts`, event name,
and compact `properties`. The eight lifecycle names are:

- `fabro.run.created`
- `fabro.run.started`
- `fabro.run.running`
- `fabro.run.completed`
- `fabro.run.failed`
- `fabro.run.cancelled`
- `fabro.run.paused`
- `fabro.run.unpaused`

Each lifecycle payload preserves the run ID, source item ID, source timestamp,
and source event name. Source IDs include the stream kind, for example
`platform:1` or `petri:coordinator/3/0`, so the two logs cannot collide.
Native records do not carry a request actor; the optional actor stays absent.
Creation preserves the initiating principal from the immutable run spec.

Creation includes the original title, workflow slug, automation reference,
Git context, and parent ID. Start includes workflow identity, goal, and Git
baseline/branch metadata when available. Terminal events include the outcome,
final Git commit, automation reference, and recorded aggregate Lithos usage
when available. Completion also retains the recorded cost in USD micros;
missing cost remains absent. Mirroring does not price usage again.

Unknown event names and their arbitrary JSON properties round-trip unchanged.
Malformed properties for a known event are rejected rather than silently
reinterpreted as an unknown event.

## Persistence

`fabro-store::MainEventStore` uses the existing run-summary SQLite pool. The
`main_events` table has:

- `seq`: a positive global sequence, bounded to `u32`;
- `source_key`: an optional unique key for idempotent source delivery;
- `event_json`: the typed main event serialized as JSON.

There is no foreign key to a run. Run deletion removes neither these events
nor their deduplication identities. A `BEGIN IMMEDIATE` transaction serializes
writers, checks for an existing source, and commits the next sequence and
payload together. Retrying a source returns the original envelope, consumes
no sequence, and emits no new live message. Failed writes publish nothing.
The next sequence comes from the committed table, including after restart.
Sequence exhaustion returns an error without overwriting existing events.

The store supports ordinary append, source-keyed append, inclusive replay
from a global sequence, and in-process subscription. Replay queries return
at most `limit + 1` rows in sequence order. Subscribers receive committed
appends; a slow subscriber must replay after a broadcast lag. Broadcasts are
local to the shared store handle, not a cross-process delivery mechanism.

Storage owns persistence and deduplication. It does not decide which run
records belong in the lifecycle stream.

## Native lifecycle mapping

`fabro-petri::main_events` maps native stream items into compact payloads:

- The platform `run.created` record supplies creation from its immutable
  record, including the original title rather than a later renamed title.
- Petri's `run.started` supplies start.
- Platform `run.lifecycle` transitions supply running, paused, unpaused,
  succeeded, failed, and dead outcomes.
- A terminal failure whose reason is `Cancelled` becomes
  `fabro.run.cancelled`. A cancellation request alone is not terminal.

Petri's `run.finished` is not mirrored independently: Fabro's terminal
lifecycle record reflects the coordinator result after required finalization.
Stage, agent, tool, transcript, sandbox, graph, settings, patch, and output
records remain exclusively in the canonical run history.

## Server delivery and recovery

The server mirrors creation synchronously after persistence and projection,
before returning success from create, retry, fork, or rewind. A failed main
append fails that request; the persisted run is retained and background
reconciliation can repair the missing delivery. The synchronous path and
background path use the same source identity.

A separate server task follows projector commit signals and scans all retained
runs at startup and every 30 seconds. For each run it reads stream pages and
loads the committed projection after the page, so terminal usage is visible.
It advances its in-memory cursor only after an item has been processed
successfully. Mapping or storage errors are logged with run identity and
retried; they do not interrupt execution or the existing live run broadcast.

After restart, cursors begin at zero. The canonical run stream acts as the
outbox, and source-keyed appends make replay idempotent. A lost or lagged signal
costs delivery latency; the periodic scan repairs it. Cursors are an
optimization, never evidence that an uncommitted append succeeded.

The main sequence describes append order. Recovery may interleave historical
runs differently from their original wall-clock order; source timestamps
preserve the time of the underlying records. Events already mirrored survive
run deletion, but a run deleted before reconciliation cannot supply missing
records afterward. This is not an exactly-once trigger-delivery contract.

## Scope and verification

This change adds no public main-stream API, CLI command, SSE endpoint, webhook
ingestion, automation listener, persistent consumer cursor, or delivery receipt
for external consumers.

Tests cover all lifecycle names and JSON round trips, unknown events and
malformed known events, native record filtering, cancellation, terminal usage,
global replay and subscription order, concurrent deduplication, restart
recovery, failed writes, sequence exhaustion, deletion survival, multi-page
reconciliation, and startup reconciliation without a new signal.
