# Fabro Event Schema V2 Proposal

Date: 2026-04-08

Status: proposal

Assumptions:

- greenfield redesign
- no production deployments
- no backward-compatibility constraints
- optimize for the best long-term public event contract

This proposal turns the earlier ideation into concrete schema changes.

## Design Goal

Fabro should expose:

1. a durable, append-only event log for audit, storage, replay, and projections
2. a separate live stream for UI-oriented snapshots, deltas, and fast progress

They should share IDs and correlation fields, but they should not be the same contract.

## Top 10 Concrete Improvements

### 1. Split the single event story into two concrete public APIs

#### Proposal

Introduce two top-level event contracts:

- `DurableEvent`
- `LiveEvent`

Endpoints:

- `GET /runs/:run_id/events`
  - append-only durable events
  - replayable
  - no keep-alive payload events
- `GET /runs/:run_id/live`
  - live UI stream
  - snapshots + deltas + keep-alives
  - resumable with cursor

#### Durable event shape

```json
{
  "kind": "durable",
  "id": "evt_01960d0c...",
  "seq": 182,
  "ts": "2026-04-08T15:01:02.123Z",
  "run_id": "run_01JQ...",
  "event": "agent.tool.started",
  "session_id": "ses_123",
  "node_id": "code",
  "properties": {
    "tool_call_id": "tool_abc",
    "tool_name": "read_file",
    "arguments": { "path": "src/main.rs" }
  }
}
```

#### Live event shape

```json
{
  "kind": "live",
  "id": "levt_01960d0d...",
  "seq": 991,
  "ts": "2026-04-08T15:01:03.000Z",
  "run_id": "run_01JQ...",
  "event": "message.delta",
  "session_id": "ses_123",
  "message_id": "msg_456",
  "part_id": "part_1",
  "properties": {
    "block_type": "text",
    "delta": "Let me check that file..."
  }
}
```

#### Why this is better

- Durable events stay stable and analyzable.
- Live events can be noisy and UI-oriented without polluting projections.
- Keeps Fabro from repeating the Claude Code / OpenCode problem of mixing control, transport, and product semantics.

### 2. Add explicit stream ordering, replay, and recovery semantics

#### Proposal

Every durable and live stream event gets:

- `seq: u64`
- SSE `id:` = `seq`
- replay semantics based on `Last-Event-ID`

Server rules:

- if `Last-Event-ID` is present and still buffered, replay `seq > cursor`
- if cursor is too old, return a structured reset event in live streams and `409 replay_reset_required` in durable streams
- durable streams never emit synthetic snapshots
- live streams may start with a `*.snapshot` event after reconnect

#### New live-only events

- `stream.heartbeat`
- `run.snapshot`
- `session.snapshot`
- `node.snapshot`
- `stream.reset_required`

#### Example `stream.reset_required`

```json
{
  "kind": "live",
  "id": "levt_01960d0e...",
  "seq": 1200,
  "ts": "2026-04-08T15:02:00.000Z",
  "run_id": "run_01JQ...",
  "event": "stream.reset_required",
  "properties": {
    "reason": "cursor_too_old",
    "expected_from_seq": 1170
  }
}
```

#### Why this is better

- Reattach behavior becomes deterministic.
- Clients no longer guess whether they missed data.
- Replay is part of the contract, not an implementation detail.

### 3. Expand the envelope into a first-class correlation model

#### Proposal

Extend the shared envelope with these optional fields:

- `workflow_id`
- `stage_id`
- `branch_id`
- `checkpoint_id`
- `session_id`
- `parent_session_id`
- `turn_id`
- `message_id`
- `part_id`
- `tool_call_id`
- `request_id`
- `causation_id`
- `correlation_id`

Rules:

- `id` is the event's own identity
- `causation_id` points to the immediate triggering event, if any
- `correlation_id` groups a whole logical operation, for example one user request or one retry attempt tree
- `request_id` is transport/API request scoped, not workflow scoped

#### Concrete change

Move these IDs out of ad hoc `properties` payloads when they are structural identifiers.

Good:

```json
{
  "event": "agent.tool.completed",
  "tool_call_id": "tool_abc",
  "message_id": "msg_456",
  "properties": {
    "tool_name": "read_file",
    "is_error": false
  }
}
```

Bad:

```json
{
  "event": "agent.tool.completed",
  "properties": {
    "tool_call_id": "tool_abc",
    "message_id": "msg_456",
    "tool_name": "read_file"
  }
}
```

#### Why this is better

- Correlation becomes universal instead of event-family-specific.
- UI and analytics consumers can join without parsing `properties`.
- Parent/child agent and retry trees become much easier to reason about.

### 4. Replace stringly state with concrete tagged unions

#### Proposal

Define explicit union types for stateful fields.

Examples:

```ts
type StopReason =
  | { type: "completed" }
  | { type: "requires_input"; request_id: string }
  | { type: "interrupted"; interrupt_reason: InterruptReason }
  | { type: "failed"; error_kind: ErrorKind }
  | { type: "retries_exhausted"; attempts: number };

type RetryStatus =
  | { type: "not_retrying" }
  | { type: "retry_scheduled"; attempt: number; next_retry_at: string }
  | { type: "retrying"; attempt: number }
  | { type: "retries_exhausted"; attempts: number };

type ApprovalStatus =
  | { type: "not_required" }
  | { type: "requested"; approval_id: string }
  | { type: "approved"; approval_id: string; actor: string }
  | { type: "denied"; approval_id: string; actor: string; reason?: string };
```

#### Concrete fields to replace

- `status`
- `reason`
- `failure_class`
- `interrupt_reason`
- `stop_reason`
- `approval_status`

#### Why this is better

- Eliminates string drift.
- Makes reducers and policy engines much safer.
- Makes test fixtures much more stable.

### 5. Standardize event family grammar across the entire product

#### Proposal

Use one lifecycle vocabulary:

- `.created`
- `.started`
- `.snapshot`
- `.delta`
- `.updated`
- `.completed`
- `.failed`
- `.cancelled`
- `.interrupted`
- `.deleted`

Apply it consistently to the same kinds of things:

- `run.*`
- `stage.*`
- `session.*`
- `turn.*`
- `message.*`
- `message.part.*`
- `tool.*`
- `command.*`
- `checkpoint.*`
- `parallel.branch.*`
- `retro.*`

#### Concrete renames

Current style is already decent, but V2 should be stricter.

Examples:

- `agent.output.start` -> `message.part.started`
- `agent.text.delta` -> `message.part.delta`
- `agent.tool.output.delta` -> `tool.output.delta`
- `agent.processing.end` -> `turn.completed` or `session.idle`, depending on actual semantics

#### Why this is better

- Consumers can infer behavior from naming alone.
- Reduces one-off event families that encode bespoke lifecycle semantics.

### 6. Introduce typed content blocks and block-level deltas

#### Proposal

Represent streamable content as typed message parts.

Base union:

```ts
type MessagePart =
  | { type: "text"; part_id: string; text: string }
  | { type: "reasoning"; part_id: string; text: string }
  | { type: "tool_call"; part_id: string; tool_call_id: string; tool_name: string; input: unknown }
  | { type: "tool_result"; part_id: string; tool_call_id: string; output: unknown; is_error: boolean }
  | { type: "patch"; part_id: string; patch_ref: string }
  | { type: "file_ref"; part_id: string; file_id: string; path: string }
  | { type: "artifact_ref"; part_id: string; artifact_id: string; label: string }
  | { type: "plan"; part_id: string; items: PlanItem[] }
  | { type: "todo"; part_id: string; items: TodoItem[] }
  | { type: "command_output"; part_id: string; command_id: string; stream: "stdout" | "stderr"; text: string };
```

Live delta event:

```json
{
  "event": "message.part.delta",
  "message_id": "msg_456",
  "part_id": "part_1",
  "properties": {
    "part_type": "text",
    "delta": "checking src/main.rs"
  }
}
```

Durable completion event:

```json
{
  "event": "message.completed",
  "message_id": "msg_456",
  "properties": {
    "parts": [
      { "type": "text", "part_id": "part_1", "text": "checking src/main.rs" }
    ]
  }
}
```

#### Why this is better

- Supports rich UI without reparsing free-form text.
- Supports structured summarization, compaction, and retro generation.
- Aligns Fabro with the best parts of Claude Sessions and pi-mono.

### 7. Make approvals, questions, and operator interventions first-class durable events

#### Proposal

Add explicit event families:

- `approval.requested`
- `approval.responded`
- `question.asked`
- `question.answered`
- `interrupt.requested`
- `interrupt.applied`
- `resume.required`
- `resume.applied`

#### Example `approval.requested`

```json
{
  "kind": "durable",
  "id": "evt_01960d0f...",
  "seq": 201,
  "ts": "2026-04-08T15:03:00.000Z",
  "run_id": "run_01JQ...",
  "session_id": "ses_123",
  "tool_call_id": "tool_abc",
  "event": "approval.requested",
  "properties": {
    "approval_id": "apr_1",
    "scope": "tool_call",
    "tool_name": "exec_command",
    "request": {
      "cmd": "git push origin branch"
    }
  }
}
```

#### Example `approval.responded`

```json
{
  "kind": "durable",
  "id": "evt_01960d10...",
  "seq": 202,
  "ts": "2026-04-08T15:03:10.000Z",
  "run_id": "run_01JQ...",
  "event": "approval.responded",
  "properties": {
    "approval_id": "apr_1",
    "result": {
      "type": "approved",
      "actor": "user"
    }
  }
}
```

#### Why this is better

- Human-in-loop behavior becomes queryable and replayable.
- Workflow interruption is no longer hidden in transport or UI state.

### 8. Add real snapshot events instead of relying on ad hoc reconstruction

#### Proposal

Define explicit snapshot events for live attach and projection recovery:

- `run.snapshot`
- `session.snapshot`
- `node.snapshot`
- `checkpoint.saved`

#### Example `session.snapshot`

```json
{
  "kind": "live",
  "id": "levt_01960d11...",
  "seq": 1500,
  "ts": "2026-04-08T15:04:00.000Z",
  "run_id": "run_01JQ...",
  "session_id": "ses_123",
  "event": "session.snapshot",
  "properties": {
    "state": { "type": "running" },
    "turn_id": "turn_9",
    "messages": [
      {
        "message_id": "msg_456",
        "role": "assistant",
        "parts": [
          { "type": "text", "part_id": "part_1", "text": "checking src/main.rs" }
        ]
      }
    ],
    "active_tool_calls": [
      {
        "tool_call_id": "tool_abc",
        "tool_name": "read_file",
        "status": "running"
      }
    ]
  }
}
```

#### Concrete rule

- snapshots are authoritative replacement state for live consumers
- snapshots are optional in durable streams
- checkpoints are durable domain snapshots, not just UI snapshots

#### Why this is better

- Fast attach becomes trivial.
- Projections can self-heal from snapshots.
- Checkpoint semantics become explicit rather than emergent.

### 9. Make model, tool, command, and MCP work first-class span families

#### Proposal

Create event families with shared semantics:

- `model.request.started`
- `model.request.completed`
- `model.request.failed`
- `tool.started`
- `tool.output.delta`
- `tool.completed`
- `tool.failed`
- `command.started`
- `command.output.delta`
- `command.completed`
- `command.failed`
- `mcp.call.started`
- `mcp.call.progress`
- `mcp.call.completed`
- `mcp.call.failed`

#### Example `model.request.completed`

```json
{
  "kind": "durable",
  "id": "evt_01960d12...",
  "seq": 220,
  "ts": "2026-04-08T15:05:00.000Z",
  "run_id": "run_01JQ...",
  "session_id": "ses_123",
  "turn_id": "turn_9",
  "request_id": "req_llm_1",
  "event": "model.request.completed",
  "properties": {
    "provider": "anthropic",
    "model": "claude-sonnet-4",
    "latency_ms": 1834,
    "usage": {
      "input_tokens": 1400,
      "output_tokens": 380,
      "reasoning_tokens": 120,
      "cache_read_tokens": 900,
      "cache_write_tokens": 0
    },
    "retry_status": { "type": "not_retrying" }
  }
}
```

#### Why this is better

- Cost and latency analysis become first-class.
- Policy engines can reason about real operations, not just stage summaries.
- Cross-provider comparison gets much easier.

### 10. Generate and enforce the public schema, docs, and examples from one registry

#### Proposal

Build a single `event_schema_registry` source that defines:

- envelope fields
- event families
- payload types
- union types
- versioning
- example payloads

Artifacts generated from it:

- Rust types
- TypeScript types
- JSON Schema
- OpenAPI / SSE docs
- sample event fixtures
- validation tests

#### Concrete rules

- every public event must have:
  - one schema definition
  - one example payload
  - one validation test
- no endpoint may inject extra consumer-visible fields outside the schema
- keep-alive frames are documented separately from payload events

#### Why this is better

- Prevents the OpenCode and Goose class of drift.
- Makes Fabro's event API publishable and stable from day one.

## Recommended V2 Event Families

If Fabro were starting from scratch, I would structure the public families like this:

- `run.*`
- `stage.*`
- `checkpoint.*`
- `parallel.branch.*`
- `session.*`
- `turn.*`
- `message.*`
- `message.part.*`
- `model.request.*`
- `tool.*`
- `command.*`
- `mcp.call.*`
- `approval.*`
- `question.*`
- `interrupt.*`
- `resume.*`
- `compaction.*`
- `retro.*`
- `artifact.*`
- `stream.*` (live only)

## Recommended Field Placement Rules

Top-level envelope:

- identity and correlation
- ordering
- timestamps
- scope

`properties`:

- event-family-specific payload
- business data
- structured state payloads

Never in `properties` if they are structural:

- `run_id`
- `seq`
- `event`
- `session_id`
- `message_id`
- `tool_call_id`
- `request_id`
- `causation_id`
- `correlation_id`

## Bottom Line

The best greenfield version of Fabro is not "the current schema plus more events."

It is:

- separate durable and live contracts
- replayable ordered streams
- a richer envelope
- typed state unions
- typed content blocks
- explicit snapshots
- first-class HITL events
- first-class span families
- generated schema/docs/tests from one registry

That would give Fabro a better event platform than any of the compared systems.
