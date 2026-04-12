# Fabro Event Schema V2: Concrete Shape

Date: 2026-04-09

Status: implemented

This document turns the settled design decisions from the event-schema discussion into a concrete wire-contract proposal.

It intentionally supersedes the earlier framing in [fabro-event-schema-v2-proposal.md](/Users/bhelmkamp/p/fabro-sh/fabro/docs-internal/fabro-event-schema-v2-proposal.md) for:

- proposal 1: one canonical persisted log, not two truths
- proposal 2: formalize and generalize the existing `since_seq` replay contract, rather than inventing replay from scratch

## Design Decisions Carried Forward

- one canonical persisted event log
- plain hand-coded Rust structs are the authoritative source of truth for the event contract
- `RunEvent` remains the canonical semantic event type
- `seq` remains outside `RunEvent`, in the store/API envelope
- replay stays built around ordered `since_seq` cursors
- typed Rust consumers matching on `EventBody` remain the primary consumer model
- the envelope widens only modestly for execution topology and tool-call correlation: `stage_id`, `parallel_group_id`, `parallel_branch_id`, `tool_call_id`
- existing durable event families stay broadly intact
- live token/delta noise does not become part of the durable persisted Rust event contract
- snapshots are out of scope for both the durable event contract and the attach API

## Contract Source Of Truth

V2 does not adopt schema generation or a registry-first workflow.

The authoritative source of truth for the event contract should be plain, hand-coded Rust structs and enums that model the public wire shape directly.

Implications:

- the Rust event types are the canonical contract
- this document describes that contract and should stay aligned with the Rust types
- any TypeScript types, JSON Schema, or OpenAPI fragments are secondary artifacts, not the source of truth
- codegen is explicitly out of scope for the initial V2 implementation

## Why Evolve The Current Model

V2 should evolve Fabro's existing event architecture rather than replace it with a generic event platform.

Earlier drafts of this document proposed a generic reducer contract, a larger ontology-first envelope, and a narrower replacement event catalog. V2 walks that back. The current code's boundary between internal workflow events, `RunEvent`, and `EventEnvelope` is stronger and simpler than it first appeared, so evolving that model is cheaper and clearer than replacing it.

The current code already has a strong separation of concerns:

- internal workflow/runtime events in `fabro-workflow`
- one canonical semantic `RunEvent`
- a store/API envelope that carries `seq` outside the event payload

That separation is worth preserving. The main V2 changes should be:

- modest envelope widening for execution topology
- cleanup and clarification of event-family boundaries
- keeping the durable event catalog semantic and typed

V2 should not introduce:

- a generic reducer contract based on `entity_type` / `event_role`
- canonical persisted token deltas
- snapshot events as a second truth layer

## Capability Coverage Decisions

V2 is evolutionary over the current `RunEvent` surface. It keeps the existing durable event families broadly intact rather than replacing them with a new ontology.

The main additions are:

- `stage_id` in the envelope for concrete stage execution identity
- `parallel_group_id` in the envelope for one execution of a parallel node
- `parallel_branch_id` in the envelope for one branch inside a parallel execution
- `tool_call_id` in the envelope for agent tool lifecycle events that need a stable cross-family join key

Everything else should remain in typed `EventBody` props unless there is a strong cross-family reason to promote it. `session_id` already exists in the envelope today and stays as-is. `tool_call_id` is promoted now because `agent.tool.*` events already carry a stable tool-call identity that other durable families can reference when needed. `turn_id` is deferred because Fabro does not yet have a durable turn identity that spans the families that would need to join on it.

## Exact Delta From Current Code

This is the implementation delta from the current Rust codebase, not the full history of how the design was reached.

### Add

- add `stage_id: Option<String>` to `RunEvent`
- add `parallel_group_id: Option<String>` to `RunEvent`
- add `parallel_branch_id: Option<String>` to `RunEvent`
- add `tool_call_id: Option<String>` to `RunEvent`
- add `actor: Option<ActorRef>` to `RunEvent`
- extend envelope extraction in `stored_event_fields()` to populate the new execution-topology fields when known
- extend envelope extraction in `stored_event_fields()` to populate `tool_call_id` on tool-lifecycle events when known
- update `RunEvent` serialization and parsing so the new optional envelope fields round-trip cleanly

### Keep As-Is

- `RunEvent` remains the canonical semantic event type
- `EventBody` remains the typed tagged union of durable event families
- `EventBody::Unknown` remains the compatibility valve for unknown event names on read
- `EventEnvelope` remains the ordered outer wrapper with `seq` outside the event payload
- `EventEnvelope.payload` remains `EventPayload`, not `RunEvent`
- the internal/store `EventEnvelope` Rust type stays wrapped as `{ seq, payload }`
- attach/replay remains exact ordered replay from `since_seq`, followed by live tailing
- current durable event families stay broadly intact
- live token/delta noise remains outside the durable persisted contract
- snapshots remain out of scope

### Do Not Do

- do not inline `seq` into `RunEvent`
- do not introduce `entity_type`, `entity_id`, or `event_role`
- do not replace typed Rust consumers with a generic reducer model
- do not redesign the store envelope
- do not add snapshot events or attach-time synthetic snapshots
- do not persist token deltas or other live UI noise as durable `RunEvent`s

## Canonical Rust Shapes

V2 should model the public contract directly as hand-coded Rust types, following the existing architecture.

```rust
pub struct RunEvent {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub run_id: RunId,
    pub node_id: Option<String>,
    pub node_label: Option<String>,
    pub stage_id: Option<String>,
    pub parallel_group_id: Option<String>,
    pub parallel_branch_id: Option<String>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub actor: Option<ActorRef>,
    pub body: EventBody,
}

pub struct EventEnvelope {
    pub seq: u32,
    pub payload: EventPayload,
}

pub struct ActorRef {
    pub kind: ActorKind,
    pub id: Option<String>,
    pub display: Option<String>,
}

pub enum ActorKind {
    User,
    Agent,
    System,
}
```

`RunEvent` remains the semantic product event. `EventEnvelope` remains the ordered store/API wrapper. The store continues to persist validated JSON `EventPayload`, not typed `RunEvent` structs.

For wire JSON, `EventEnvelope` should serialize in flattened form so clients see:

```json
{
  "seq": 4861,
  "id": "...",
  "ts": "...",
  "run_id": "...",
  "event": "...",
  "properties": { ... }
}
```

That flattening is a wire concern only. It does not move `seq` into `RunEvent`, and it does not change the internal/store Rust shape of `EventEnvelope`.

`EventBody` remains a hand-coded tagged enum serialized as:

```json
{
  "event": "stage.completed",
  "properties": { "...": "..." }
}
```

V2 should also preserve the current unknown-event fallback shape:

```rust
EventBody::Unknown {
    name: String,
    properties: serde_json::Value,
}
```

This fallback already exists in the current code and should be kept.

### Envelope Rules

- `id`, `ts`, `run_id`, and `event` are always present on the serialized `RunEvent`.
- `seq` is not part of `RunEvent`. It stays in the outer `EventEnvelope`.
- Optional envelope fields are omitted, never serialized as `null`.
- The existing top-level envelope fields remain:
  - `node_id`
  - `node_label`
  - `session_id`
  - `parent_session_id`
- V2 adds only these new optional envelope fields:
  - `stage_id`
  - `parallel_group_id`
  - `parallel_branch_id`
  - `tool_call_id`
- Other relationship identifiers stay inside typed `properties`.
- `turn_id` remains in typed `properties`; see the deferral decision in `Capability Coverage Decisions`.
- `actor` is optional. When present, it identifies the primary actor for the event.
- Set `actor` on human- or agent-initiated events where that identity matters to consumers. Example: `run.cancel.requested` should identify the user who initiated the cancel.
- Set `actor` on durable agent output when the producing session identity matters. Example: `agent.message` should identify the agent session.
- Omit `actor` for routine runtime events with no meaningful primary actor. Example: `stage.started`.

### ID Format Conventions

- `run_id` keeps Fabro's current format: an unprefixed ULID string.
- `stage_id` keeps Fabro's current format: `"{node_id}@{visit}"`.
- `node_id` is the stable graph node identifier from the workflow definition.
- `parallel_group_id` should be the durable identity of one execution of a parallel node. The default format should be `"{node_id}@{visit}"`.
- `parallel_branch_id` should be the durable identity of one branch within a parallel execution. The default format should be `"{parallel_group_id}:{index}"`.
- Consumers should otherwise treat IDs as opaque strings.

### Presence Expectations

- `stage_id` is present on events tied to a concrete stage execution.
- `parallel_group_id` is present on `parallel.*` events and on events emitted inside a parallel execution when that scope is known.
- `parallel_branch_id` is present on `parallel.branch.*` events and on nested events emitted inside a specific branch when that scope is known.
- `session_id` and `parent_session_id` keep their current meaning for forwarded agent/session activity.
- `tool_call_id` is present on `agent.tool.*` events and on other durable events that directly describe the same tool call.
- `node_label` remains in the envelope for display-oriented consumers.
- `actor` is expected on control actions and durable agent output when there is a meaningful user or agent identity to expose. It is usually omitted on routine runtime lifecycle events.

## Consumer Model

Rust consumers should keep matching on `RunEvent.body` using typed `EventBody` variants.

This document does not adopt:

- `entity_type`
- `entity_id`
- `event_role`
- a generic reducer contract

External JSON consumers should continue to:

- match on `"event"`
- read event-specific values from `"properties"`
- read `"seq"` from the flattened outer event envelope on API/SSE responses
- use envelope metadata only for cross-cutting context such as stage, session, execution topology, and tool-call correlation

## Replay Contract

Fabro keeps the current replay model:

- ordered events are stored as `EventEnvelope { seq, payload }`
- API/SSE serialization of `EventEnvelope` should flatten `seq` into the top-level JSON object returned to clients
- attach starts from `since_seq`
- the server replays exact persisted envelopes and then tails live envelopes while the run is active
- SSE keepalive comments are transport frames, not events

V2 does not introduce:

- `run.snapshot`
- `session.snapshot`
- API-level attach snapshots
- persisted snapshot events of any kind

The durable model remains simple: replay ordered events, no duplicate truth layer.

## Implementation Checklist

An engineer implementing this proposal should make only these structural changes unless a later section explicitly says otherwise.

1. Update [`RunEvent`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/mod.rs) to add:
   - `stage_id`
   - `parallel_group_id`
   - `parallel_branch_id`
   - `tool_call_id`
   - `actor`
2. Update `RunEvent::to_value()` and `RunEvent` parsing in [`run_event/mod.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/mod.rs) so the new envelope fields serialize and deserialize.
3. Extend `StoredEventFields` and `stored_event_fields()` in [`event.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-workflow/src/event.rs) to populate:
   - `stage_id`
   - `parallel_group_id`
   - `parallel_branch_id`
   - `tool_call_id` on tool-lifecycle events
   - `actor` when there is a clear primary actor
   These values should come from the emitter's current execution context for stage and parallel scope, and from event-specific payloads for `tool_call_id`.
4. Leave [`EventEnvelope`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-store/src/types.rs) structurally unchanged:
   - `seq: u32`
   - `payload: EventPayload`
5. Update API/SSE envelope serialization so wire JSON is flattened:
   - top-level `seq`
   - then the `RunEvent` payload fields alongside it
   - no `"payload": { ... }` wrapper in JSON responses
6. Leave the replay/attach flow unchanged in behavior:
   - persisted replay from `since_seq`
   - live tail after replay
   - no snapshots
7. Keep the current `EventBody` family surface unless there is an explicit product reason to change a specific family.
8. Keep streaming-noise agent events out of durable `RunEvent` conversion.
9. Update the HTTP/API schema docs to reflect both:
   - new `RunEvent` envelope fields
   - flattened JSON serialization of `EventEnvelope`

## EventBody And Property Model

V2 should keep the current hand-coded domain split for prop structs:

- run props in [`run.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/run.rs)
- stage and checkpoint props in [`stage.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/stage.rs)
- agent props in [`agent.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/agent.rs)
- infra/setup/devcontainer props in [`infra.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/infra.rs)
- parallel/interview/git/misc props in [`misc.rs`](/Users/bhelmkamp/p/fabro-sh/fabro/lib/crates/fabro-types/src/run_event/misc.rs)

That split is part of the design quality. V2 should keep adding hand-coded prop structs, not collapse everything into generic maps.

## Durable Event Surface

V2 keeps the current durable family surface broadly intact.

### Run

- `run.created`
- `run.started`
- `run.submitted`
- `run.starting`
- `run.running`
- `run.removing`
- `run.cancel.requested`
- `run.pause.requested`
- `run.unpause.requested`
- `run.paused`
- `run.unpaused`
- `run.rewound`
- `run.completed`
- `run.failed`
- `run.notice`

### Stage And Prompt

- `stage.started`
- `stage.completed`
- `stage.failed`
- `stage.retrying`
- `stage.prompt`
- `prompt.completed`

### Parallel

- `parallel.started`
- `parallel.branch.started`
- `parallel.branch.completed`
- `parallel.completed`

### Interview / Human Input

- `interview.started`
- `interview.completed`
- `interview.timeout`
- `interview.interrupted`

### Checkpoint

- `checkpoint.completed`
- `checkpoint.failed`

### Agent Durable Events

- `agent.session.started`
- `agent.session.ended`
- `agent.processing.end`
- `agent.input`
- `agent.message`
- `agent.tool.started`
- `agent.tool.completed`
- `agent.error`
- `agent.warning`
- `agent.loop.detected`
- `agent.turn.limit`
- `agent.steering.injected`
- `agent.compaction.started`
- `agent.compaction.completed`
- `agent.llm.retry`
- `agent.sub.spawned`
- `agent.sub.completed`
- `agent.sub.failed`
- `agent.sub.closed`
- `agent.mcp.ready`
- `agent.mcp.failed`
- `agent.failover`

### Git

- `git.commit`
- `git.push`
- `git.branch`
- `git.worktree.added`
- `git.worktree.removed`
- `git.fetch`
- `git.reset`

### Infra And Execution

- `sandbox.*`
- `setup.*`
- `cli.ensure.*`
- `command.*`
- `agent.cli.*`
- `devcontainer.*`
- `pull_request.*`
- `artifact.captured`
- `ssh.ready`
- `subgraph.*`
- `edge.selected`
- `loop.restart`
- `retro.*`

## Explicitly Non-Durable Streaming Noise

The current boundary that keeps live token/delta noise out of `RunEvent` should remain in place.

These stay outside the durable persisted contract:

- `agent.output.start`
- `agent.output.replace`
- `agent.text.delta`
- `agent.reasoning.delta`
- `agent.tool.output.delta`
- `agent.skill.expanded`

`agent.skill.expanded` stays in this non-durable bucket because it is display-oriented expansion metadata, not a durable workflow fact.

If Fabro needs those for UI, they belong in a separate transient stream, not in the canonical persisted Rust event contract.

## Example Shapes

### Flattened Wire JSON

```json
{
  "seq": 4861,
  "id": "evt_01JSE1N7RJD1NW2JSDT3W0YQ92",
  "ts": "2026-04-08T16:21:11.106Z",
  "run_id": "01JSE1M0Q0P8P6KQW9Q6D58Q0E",
  "event": "agent.tool.completed",
  "stage_id": "code@1",
  "node_id": "code",
  "node_label": "Code",
  "session_id": "ses_child",
  "tool_call_id": "call_1",
  "parent_session_id": "ses_parent",
  "properties": {
    "tool_name": "read_file",
    "output": {
      "summary": "Read docs-internal/events-strategy.md"
    },
    "is_error": false,
    "visit": 1
  }
}
```

In Rust, `EventEnvelope` still remains `{ seq, payload: EventPayload }`. The example above is only the flattened API/SSE JSON form of that envelope.

## Practical Guidance

- Preserve the current one-time canonicalization boundary from internal `Event` to external `RunEvent`.
- Keep `RunEvent` semantic and typed. Do not turn it into a generic reducer envelope.
- Keep `seq` outside the event payload.
- Widen the envelope only modestly: `stage_id`, `parallel_group_id`, `parallel_branch_id`, and `tool_call_id`.
- Keep `session_id` as the existing top-level session field.
- Keep event-specific detail inside typed props.
- Preserve `EventBody::Unknown` as the compatibility valve for unknown event names on read.
- Do not store token deltas or other live UI noise as durable `RunEvent`s.
- Do not add snapshot events or attach-time synthetic snapshots.
- When adding a new durable event, update the current Rust boundary cleanly:
  - internal `Event`
  - `event_name()`
  - envelope extraction
  - `EventBody`
  - typed props
  - affected consumers

## Open Follow-Up

- `correlation_id`-style cross-entity grouping remains deferred until Fabro has a concrete consumer and explicit propagation rules
