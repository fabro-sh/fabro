# Fabro Run Stream Strategy

A run's history is two logs, and its public event API is one ordered stream
over both:

- **Petri's records.** The engine writes every fact about execution: the run
  starting and finishing, each step's firing, progress, output and outcome, a
  question asked and answered, a scope acquired. Fabro stores them unchanged
  in `petri_records` through `fabro-petri`'s `SqliteRunStore`, and reads them
  through Petri's event contract (`RunEvent`, with its `derived` view).
- **Platform records.** Facts Fabro knows and Petri does not: the lifecycle
  before and after the engine (`run.created`, `run.lifecycle`, `run.title`,
  `run.parent`, `run.archived`, `run.superseded`, `run.notice`), who answered
  a question (`interview.answered`), the branch and git identity a run works
  under, a checkpoint commit with its diff, a collected artifact
  (`artifact.collected`), the run's diff (`run.diff`), the pull request
  requests and outcomes, a notification sent, a pairing. They are
  `PlatformRecord` values in
  `fabro-store::platform_records`, stored in `platform_records` with a
  per-run `seq`.

The **projector** (`fabro-petri::projection`) folds both logs into the run's
`RunProjection`, the view `GET /runs/{id}/state` serves, and assigns each
record it consumes a `stream_seq` in `petri_stream`. That stream is what
`GET /runs/{id}/events` and the attach stream serve, item by item, as
`RunStreamItem`: `{run_id, stream_seq, kind: petri|platform, id, recorded_at,
item}`. `stream_seq` is the cursor a client resumes from; `id` is the item's
own identity (`<log>/<seq>/<index>` for a Petri event, the record's `seq` for
a platform record) for deduplication.

Tracing logs are separate. Tracing is developer diagnostics; the stream is
the product-facing record other systems consume. If something must be
visible after a reattach, it has to be a record, not a log line.

## Recording a fact

Petri's own facts need nothing from Fabro: the engine records them and the
projector's fold reads them. Add Fabro code only for a fact Petri cannot
know.

To record such a fact:

1. Add a variant to `PlatformRecord` and its kind to `PlatformRecordKind` in
   `lib/components/fabro-store/src/platform_records.rs`. The kind is the
   `kind` tag on the wire, lowercase dot notation (`pull_request.created`).
   A record that belongs to a stage names its `execution` and `firing`.
2. Append it through the server's `run_records` module (or the worker's
   client), which commits the record and wakes the projector. Never write
   `platform_records` from anywhere else.
3. Fold it in `fabro-petri::projection` when the projection should show it.
   A record nobody reads from the projection still reaches the stream.
4. Update the readers that match on record kinds: the CLI's pretty stream
   rendering (`petri_stream.rs`), the web app's stream handling, the Slack
   service, and the tests or fixtures that name kinds.

Do not add a platform record that restates a Petri record. The projection
already carries what the engine knows; read it there.

## Reading the stream

Servers and workers hold the stream through the projector: `stream_after`
for a page, `subscribe` for live items, `stream_head` for the cursor to
start from. The server's `stream_follower` reads every run's stream once and
fans it out to the in-memory run map and the global broadcast that `/attach`
and the Slack service take their items from.

Clients read `GET /runs/{id}/events?after=<stream_seq>` for a page and the
attach stream for live items; `fabro-client` exposes `list_run_stream`,
`list_run_stream_until` and `attach_run_stream`.

When matching items:

- A Petri event's name is `item.record.body.event` (`run.started`,
  `step.started`, `step.progress.recorded`, `step.finished`,
  `run.finished`); its parsed meaning is under `item.derived` (a pending
  question is `derived.parsed.kind == "question"`).
- A platform record's kind is `item.record.kind`.
- The run has ended when a platform `run.lifecycle` record's `transition`
  is `succeeded`, `failed` or `dead`. Petri's `run.finished` precedes it and
  carries the engine's own status.

Never rebuild an item downstream: pass the `RunStreamItem` through as read.

## Agent events

Pebble's `CodingAgentEvent` stream is the agent event contract. Petri stores
each event a coding agent publishes for a step as that step's progress, and
the projector folds them into `StageProjection.agent` with pebble's
`SessionProjection`. Read `StageProjection.agent`, or the stored progress
record itself, instead of folding the stream again. Fabro adds nothing of
its own to this stream.

## Ask Fabro sessions

Ask Fabro sessions are not runs. Their events (`run.session.*`) live in
their own log, `run_session_events`, through `RunSessionEventStore`, numbered
per session and served by the sessions API. They never enter a run's stream.

## Persistence guarantees

A record is committed before it is visible: the projector reads only what
the store has committed, and the stream's `stream_seq` is assigned in the
same transaction as the projection that consumed the record. A client that
resumes from its last `stream_seq` sees every item exactly once.

A worker cannot continue past a record it failed to append: the store's
error reaches the engine and fails the run.
