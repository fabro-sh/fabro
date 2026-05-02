# Fabro Events Strategy

Fabro emits structured **workflow run events** during execution for observability. Events are the durable audit trail for a run: they drive the run store, SSE streaming, CLI progress rendering, retro analysis, and optional JSONL sinks.

Events are distinct from tracing logs. Tracing is developer diagnostics; events are product-facing state transitions and activity records that other systems consume.

Detached runs rely on this distinction. If something needs to be visible after reattach, emit a `Event` rather than only logging to stderr or `detach.log`.

## Architecture

```text
Engine/Handler -> Event -> Emitter::emit()
                                             |- trace(raw event)
                                             |- canonicalize -> RunEvent
                                             `- on_event(&RunEvent)
                                             |- run store
                                             |- SSE
                                             |- optional JSONL/debug sinks
                                             `- CLI / tests / metrics listeners
```

The canonical `RunEvent` is built exactly once in `fabro-workflow/src/event.rs`.

- `Event` (in `fabro-workflow`) is the internal typed event emitted by engine and handlers.
- `Emitter` owns an immutable `run_id` and converts `Event` into `RunEvent` via `to_run_event_at()`.
- `RunEvent` (in `fabro-types`) holds envelope metadata plus a typed `body: EventBody`. It has no cached JSON fields; the wire format is produced only during serialization.
- Every listener receives `&RunEvent`, not `&Event`.
- Bypass paths that cannot go through the emitter must call `to_run_event()` once and reuse the same `RunEvent` for every sink.

## Canonical Envelope

Each serialized `RunEvent` uses this canonical envelope:

```json
{
  "id": "01960d0c-5d16-7d6e-8f61-9fd6f4a532b5",
  "ts": "2026-03-30T12:00:01.000Z",
  "run_id": "01JQ...",
  "event": "agent.tool.started",
  "session_id": "ses_child",
  "parent_session_id": "ses_parent",
  "node_id": "code",
  "node_label": "Code",
  "actor": {
    "kind": "agent",
    "session_id": "ses_child",
    "parent_session_id": "ses_parent",
    "model": "gpt-5.2"
  },
  "properties": {
    "tool_name": "read_file",
    "tool_call_id": "call_1",
    "arguments": {"path": "src/main.rs"}
  }
}
```

Always-present fields:

| Field | Type | Notes |
|---|---|---|
| `id` | string | UUIDv7 event id |
| `ts` | string | UTC timestamp with millisecond precision |
| `run_id` | string | Workflow run id |
| `event` | string | Lowercase dot-notation event name |

Optional top-level fields:

| Field | When present |
|---|---|
| `session_id` | Agent/session events |
| `parent_session_id` | Forwarded child-session events |
| `node_id` | Events tied to a graph node or branch |
| `node_label` | Display label for `node_id`; omitted when not applicable |
| `actor` | The principal responsible for the event |

Everything else lives inside `properties`.

Important rules:

- Optional envelope fields are omitted, not serialized as `null`.
- Event-specific fields do not get flattened into the top level.
- Actor identity lives only in top-level `actor: Principal`; never duplicate it in event-specific properties.
- User actors must carry canonical IdP identity through `Principal::User { identity, login, auth_method }`, not a login-only string.
- `EventPayload` validation requires `id`, `ts`, `run_id`, and `event`.

## Naming

The external event name is lowercase dot notation, for example:

- `run.started`
- `stage.completed`
- `agent.tool.started`
- `sandbox.ready`
- `parallel.branch.completed`

`event_name()` in `event.rs` is exhaustive. Do not use wildcard fallthroughs when adding new variants.

## Node And Session Metadata

`node_id` is the stable graph identifier. `node_label` is the human-facing display name. Stage events should surface both through the envelope when applicable.

Agent events now use explicit session links:

- `session_id` identifies the session that originally emitted the event.
- `parent_session_id` identifies the immediate parent session for forwarded child events.
- Nested sub-agents preserve immediate parentage across boundaries.

`AgentEvent::SubAgentEvent` no longer exists. Child activity is forwarded as normal agent events with session linkage in the envelope.

## Direct-Write Paths

Most events flow through `Emitter::emit()`. The remaining direct-write paths must use:

1. `to_run_event(run_id, event)`
2. Serialize and redact once
3. Reuse that exact `RunEvent` for every sink

Never build the same `RunEvent` twice if multiple sinks receive it.

## Adding A New Event

### 1. Add the typed event

Add a variant to `Event`, `AgentEvent`, or `SandboxEvent` as appropriate.

### 2. Add tracing

Extend `Event::trace()` so the raw event is observable in tracing output.

### 3. Add an external name

Extend `event_name()` with the new lowercase dot-notation string.

### 4. Add the `EventBody` variant

Add a variant to `EventBody` in `fabro-types/src/run_event/mod.rs` with a corresponding props struct. Use `#[serde(rename = "dotted.name")]` matching the external name from step 3.

### 5. Map envelope fields and construct `EventBody`

Update `stored_event_fields()` and `event_body_from_event()` in `fabro-workflow/src/event.rs`:

- Move `node_id`, `node_label`, `session_id`, and `parent_session_id` into the envelope when appropriate.
- Construct the `EventBody` variant directly from the `Event` fields.
- For `Event::Agent` sub-variants, merge `visit` into the inner props and lift `stage` to `node_id`.
- For `Event::Sandbox` sub-variants, unwrap and flatten into the corresponding `EventBody` variant.

### 6. Emit it

Prefer `Emitter::emit(&Event::...)`.

Use `to_run_event()` only for true bypass paths.

### 7. Update consumers

Check:

- CLI progress parsing
- `fabro logs`
- retro duration extraction
- store validation
- tests or fixtures that inspect event names or fields

## Consumer Guidance

When writing Rust consumers (listeners, store projections, CLI progress):

- Match on `event.body` using `EventBody::*` variants. This gives you typed access to event-specific fields.
- Use `event.node_id`, `event.node_label`, `event.session_id`, and `event.parent_session_id` for envelope metadata.
- Only use `event.event_name()` or `event.properties()` for generic/display purposes (logging, forwarding). These involve serialization and should not be used on hot paths.

When writing external JSON consumers (SSE clients, JSONL parsers):

- Match on the `"event"` field for the dot-notation event name.
- Read event-specific data from `"properties"`.
- Read stage/branch identity from `"node_id"` and `"node_label"`.
- Read agent hierarchy from `"session_id"` and `"parent_session_id"`.

Do not rebuild or mutate the `RunEvent` in downstream listeners.

## Bypass And Persistence Guarantees

Any JSONL sink, the run store, and SSE should reflect the same canonical envelope bytes after redaction.

`status.json` remains the authoritative completion signal for detached runs. Terminal run status should only be written after all post-run work is finished.
