# Server-Side Run Pairing API And Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add server-side run pairing APIs and typed run events so a user can join one active API-mode workflow agent session, exchange multiple messages, inspect a compact transcript, and explicitly end pairing before the workflow resumes.

**Architecture:** Pairing is run-native, not MCP-native. The HTTP API is resource-shaped under `/api/v1/runs/{id}/pair`, runtime control extends the existing steering/session-control path, and observation is projected from durable typed `RunEvent`s rather than a new store. The v1 pair API is single-target, synchronously confirms pair start/end at the runtime boundary, and uses **message** for user input, never **turn**, because pair messages are inputs inside an open collaboration mode, not bounded request/response execution units.

**Tech Stack:** Rust, Axum, OpenAPI/progenitor, `fabro-types`, `fabro-api`, `fabro-client`, `fabro-server`, `fabro-workflow`, `fabro-agent`, `fabro-store`, `cargo nextest`.

---

## Scope

In scope:

- Server-side HTTP API for pair lifecycle, pair messages, compact transcript, and a generic run-event detail endpoint.
- Typed pair-related `RunEvent` variants and event conversion/storage metadata.
- Runtime behavior for one selected API-mode agent session: start pairing, park on natural completion while paired, accept pair messages, and resume after explicit end.
- Rust client convenience methods for the new API.

Out of scope:

- MCP tool surface.
- Frontend/UI changes.
- Removing or refactoring the existing `/api/v1/sessions` API.
- Non-API-mode pairing for ACP/CLI agent backends.
- Multi-target pairing. It can be added later with explicit fan-out semantics, but v1 must never broadcast pair messages by default.

## Contract Decisions

- **Single-target v1:** `POST /api/v1/runs/{id}/pair` requires a target selector copied from `GET /api/v1/runs/{id}/pair`. The server must not pair every active target automatically.
- **Synchronous lifecycle confirmation:** `POST /pair` waits for the runtime to install pair mode for the selected target and returns `200` with `status: "active"`. `DELETE /pair/{pair_id}` waits for the runtime to exit pair mode and returns `200` with `status: "ended"`.
- **Small state machine:** `PairStatus` is only `active`, `ended`, or `failed`. There is no public `requested` or `ending` state and no timeout-based read-side reconciliation helper.
- **Event-backed observation:** Pair records and transcript rows are reconstructed from durable run events. Lifecycle events are run-scoped; conversation effects are agent/session-scoped.
- **No duplicate status surfaces:** `PairRecord.status` is the source of truth for lifecycle. `PairTarget` does not carry its own lifecycle status.
- **No new session capability:** Pairing uses the existing API-mode steering-capable session control surface. Do not add `SessionCapability::Pair` unless a future backend supports steering but cannot support pairing.
- **Simple GET status:** `GET /pair` returns `run_id`, `current_pair`, and available `targets`. It does not expose a separate `pairable` boolean or reason enum; clients can derive pairability from `current_pair == null && targets.length > 0`.
- **Transcript stays in v1:** The transcript endpoint remains pair-scoped because it is core to pairing. It is a compact projection over `RunEvent`s and uses source event sequence cursors.
- **Generic details:** Pair transcript entries point to `GET /api/v1/runs/{id}/events/{seq}` for full detail. There is no pair-specific detail endpoint in v1.

## Public API Contract

Add these paths to `docs/public/api-reference/fabro-api.yaml`, regenerate `fabro-api`, and add `fabro-client` convenience methods.

### Shared Types

`PairId`

```yaml
type: string
description: Durable run pair identifier.
example: 01HZX6M29F1CD5YYMHT1F5D7WQ
```

`PairMessageId`

```yaml
type: string
description: Durable pair message identifier.
example: 01HZX6M4D7Y1QW0Q0P6V8Z4DR5
```

`PairStatus`

```yaml
type: string
enum: [active, ended, failed]
```

Status meaning:

- `active`: the runtime installed pair mode for the selected target, recorded `run.pair.started`, and recorded the `human_joined` system message inside that pair window.
- `ended`: the runtime exited pair mode, or the paired run/session ended, and recorded `run.pair.ended`.
- `failed`: a pair that was already active ended abnormally and recorded `run.pair.failed`.

`PairTargetSelector`

```json
{
  "stage_id": "code@1",
  "agent_session_id": "ses_01"
}
```

Required fields:

- `stage_id`
- `agent_session_id`

Both fields are required even if `agent_session_id` is unique within a run. `stage_id` is a stale-client cross-check so the server can reject a request when the client selected a session from one active stage but submits a selector for another.

`PairTarget`

```json
{
  "stage_id": "code@1",
  "node_id": "code",
  "node_label": "Code",
  "visit": 1,
  "agent_session_id": "ses_01",
  "provider": "openai",
  "model": "gpt-5.3"
}
```

Required fields:

- `stage_id`
- `node_id`
- `node_label`
- `visit`
- `agent_session_id`

Nullable/optional fields:

- `provider`
- `model`

Use `targets` only for available target lists. Use singular `target` on `PairRecord` so v1 cannot accidentally imply multi-target pairing. `PairTarget` intentionally has no status field; the parent `PairRecord.status` is the lifecycle source of truth. Existing raw run event envelopes may still use top-level `session_id`.

`PairRecord`

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "status": "active",
  "started_at": "2026-05-18T12:00:01Z",
  "ended_at": null,
  "failure_reason": null,
  "target": {
    "stage_id": "code@1",
    "node_id": "code",
    "node_label": "Code",
    "visit": 1,
    "agent_session_id": "ses_01",
    "provider": "openai",
    "model": "gpt-5.3"
  }
}
```

Required fields:

- `pair_id`
- `run_id`
- `status`
- `started_at`
- `target`

Nullable fields:

- `ended_at`
- `failure_reason`

`RunPairStatusResponse`

```json
{
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "current_pair": null,
  "targets": [
    {
      "stage_id": "code@1",
      "node_id": "code",
      "node_label": "Code",
      "visit": 1,
      "agent_session_id": "ses_01",
      "provider": "openai",
      "model": "gpt-5.3"
    }
  ]
}
```

Required fields:

- `run_id`
- `targets`

Nullable field:

- `current_pair`

`PairStartRequest`

```json
{
  "target": {
    "stage_id": "code@1",
    "agent_session_id": "ses_01"
  }
}
```

Required fields:

- `target`
- `target.stage_id`
- `target.agent_session_id`

The target selector must match exactly one active target returned by `GET /pair`.

`PairMessageRequest`

```json
{
  "text": "Can you inspect the failing test?",
  "client_message_id": "optional-correlation-id"
}
```

Validation:

- `text` is required.
- `text` must be trimmed non-empty.
- `text` max length is `8192`.
- `client_message_id` is optional and echoed if present.
- `client_message_id` is not a deduplication key in v1; it is a caller correlation field only.

`PairMessageRecord`

```json
{
  "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
  "client_message_id": "optional-correlation-id",
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "target": {
    "stage_id": "code@1",
    "agent_session_id": "ses_01"
  },
  "text": "Can you inspect the failing test?",
  "accepted_at": "2026-05-18T12:01:00Z"
}
```

Required fields:

- `message_id`
- `pair_id`
- `run_id`
- `target`
- `text`
- `accepted_at`

Nullable field:

- `client_message_id`

### `GET /api/v1/runs/{id}/pair`

Returns the active pair if any and all currently active targets eligible for pairing.

Success `200`: `RunPairStatusResponse`.

Example:

```json
{
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "current_pair": {
    "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
    "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
    "status": "active",
    "started_at": "2026-05-18T12:00:01Z",
    "ended_at": null,
    "failure_reason": null,
    "target": {
      "stage_id": "code@1",
      "node_id": "code",
      "node_label": "Code",
      "visit": 1,
      "agent_session_id": "ses_01",
      "provider": "openai",
      "model": "gpt-5.3"
    }
  },
  "targets": [
    {
      "stage_id": "review@1",
      "node_id": "review",
      "node_label": "Review",
      "visit": 1,
      "agent_session_id": "ses_02",
      "provider": "openai",
      "model": "gpt-5.3"
    }
  ]
}
```

Errors:

- `404` `ErrorResponse`: run not found.

### `POST /api/v1/runs/{id}/pair`

Starts pairing with one selected active pairable API-mode agent target.

Request body: `PairStartRequest`.

```json
{
  "target": {
    "stage_id": "code@1",
    "agent_session_id": "ses_01"
  }
}
```

Success `200`: `PairRecord` with `status: "active"` after the runtime installs pair mode for the target, records `run.pair.started`, queues the `human_joined` system message, and requests interruption/parking.

The successful response does not mean the model has produced a follow-up assistant message. It means the selected runtime session is now in pair mode and will not naturally complete the workflow stage until pairing ends.

Example:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "status": "active",
  "started_at": "2026-05-18T12:00:01Z",
  "ended_at": null,
  "failure_reason": null,
  "target": {
    "stage_id": "code@1",
    "node_id": "code",
    "node_label": "Code",
    "visit": 1,
    "agent_session_id": "ses_01",
    "provider": "openai",
    "model": "gpt-5.3"
  }
}
```

Errors:

- `400` blank or missing target selector.
- `404` run not found.
- `409` with `code: "run_not_pairable"` when the run is not running, is blocked, is terminal, has no active API session, or only has non-pairable active agents.
- `409` with `code: "already_paired"` when a current pair exists.
- `409` with `code: "pair_target_not_active"` when the requested target no longer exists or is not active.
- `409` with `code: "pair_target_not_pairable"` when the target exists but is no longer an active API-mode steering-capable target.
- `503` with `code: "worker_control_unavailable"` when the live worker control channel is missing, closed, times out before confirmation, or cannot return a runtime-level confirmation.

### `GET /api/v1/runs/{id}/pair/{pair_id}`

Returns a current or historical pair reconstructed from durable events.

Success `200`: `PairRecord`.

Errors:

- `404` with `code: "pair_not_found"` when no pair with that id exists for the run.
- `404` run not found.

### `DELETE /api/v1/runs/{id}/pair/{pair_id}`

Ends that exact pair. The path `pair_id` is a stale-write guard.

Request body: none.

Success `200`: `PairRecord` with `status: "ended"` after the runtime exits pair mode, queues the `human_left` system message when the target session is still present, and records `run.pair.ended`.

Example:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "status": "ended",
  "started_at": "2026-05-18T12:00:01Z",
  "ended_at": "2026-05-18T12:05:00Z",
  "failure_reason": null,
  "target": {
    "stage_id": "code@1",
    "node_id": "code",
    "node_label": "Code",
    "visit": 1,
    "agent_session_id": "ses_01",
    "provider": "openai",
    "model": "gpt-5.3"
  }
}
```

Errors:

- `404` run not found.
- `404` with `code: "pair_not_found"`.
- `409` with `code: "pair_not_current"` when the pair exists but is not the current pair.
- `409` with `code: "pair_not_active"` when the pair is already ended or failed.
- `503` with `code: "worker_control_unavailable"` when the live worker control channel is missing, closed, times out before confirmation, or cannot return a runtime-level confirmation.

### `POST /api/v1/runs/{id}/pair/{pair_id}/messages`

Sends one paired user message to the active target. This does not create a turn and does not stream.

Request: `PairMessageRequest`.

```json
{
  "text": "Can you inspect the failing test?",
  "client_message_id": "optional-correlation-id"
}
```

Success `202`: `PairMessageRecord`.

`202` means the active target runtime accepted the message into the paired session queue and recorded `agent.pair.user_message`. If the target is gone, the queue is full, the pair is not active, or the runtime rejects the message, the API must fail the request instead of returning an accepted message that can later be silently dropped.

```json
{
  "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
  "client_message_id": "optional-correlation-id",
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "target": {
    "stage_id": "code@1",
    "agent_session_id": "ses_01"
  },
  "text": "Can you inspect the failing test?",
  "accepted_at": "2026-05-18T12:01:00Z"
}
```

Errors:

- `400` blank `text` or other semantic input error.
- `404` run not found.
- `404` with `code: "pair_not_found"`.
- `409` with `code: "pair_not_current"`.
- `409` with `code: "pair_not_active"`.
- `409` with `code: "pair_target_not_active"` when the paired target session is gone.
- `409` with `code: "pair_message_not_accepted"` when the active runtime rejects the message, including queue-cap rejection.
- `503` with `code: "worker_control_unavailable"`.

### `GET /api/v1/runs/{id}/pair/{pair_id}/transcript`

Query parameters:

- `since_seq`: integer, minimum `1`, default `1`.
- `limit`: integer, minimum `1`, maximum `1000`, default `100`.

Returns a compact projection over durable run events. Exclude deltas and full tool output.

Success `200`:

```json
{
  "data": [
    {
      "kind": "user_message",
      "seq": 42,
      "event_id": "01960d0c-5d16-7d6e-8f61-9fd6f4a532b5",
      "ts": "2026-05-18T12:01:00Z",
      "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
      "target": {
        "stage_id": "code@1",
        "node_id": "code",
        "node_label": "Code",
        "visit": 1,
        "agent_session_id": "ses_01"
      },
      "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
      "client_message_id": "optional-correlation-id",
      "text": "Can you inspect the failing test?"
    },
    {
      "kind": "assistant_message",
      "seq": 44,
      "event_id": "01960d0d-0000-7000-8000-000000000000",
      "ts": "2026-05-18T12:01:12Z",
      "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
      "target": {
        "stage_id": "code@1",
        "node_id": "code",
        "node_label": "Code",
        "visit": 1,
        "agent_session_id": "ses_01"
      },
      "text": "I found one failing test.",
      "model": {
        "provider": "openai",
        "model_id": "gpt-5.3",
        "speed": null
      },
      "tool_call_count": 0
    },
    {
      "kind": "tool_call",
      "seq": 45,
      "event_id": "01960d0e-0000-7000-8000-000000000000",
      "ts": "2026-05-18T12:01:20Z",
      "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
      "target": {
        "stage_id": "code@1",
        "node_id": "code",
        "node_label": "Code",
        "visit": 1,
        "agent_session_id": "ses_01"
      },
      "tool_call_id": "call_7",
      "tool_name": "shell",
      "status": "completed",
      "summary": "Ran cargo nextest; one test failed.",
      "is_error": false,
      "truncated": true,
      "detail_ref": {
        "seq": 45,
        "tool_call_id": "call_7"
      }
    }
  ],
  "meta": {
    "next_since_seq": 46,
    "has_more": false
  }
}
```

Transcript entry variants:

- `user_message`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `message_id`, nullable `client_message_id`, `text`.
- `system_message`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `system_message_kind`, `text`.
- `assistant_message`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `text`, `model`, `tool_call_count`.
- `tool_call`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `tool_call_id`, `tool_name`, `status`, `summary`, `is_error`, `truncated`, `detail_ref`.
- `error`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `message`, `detail_ref`.
- `warning`: `kind`, `seq`, `event_id`, `ts`, `pair_id`, `target`, `warning_kind`, `message`, `detail_ref`.

Projection rules:

- Include `agent.pair.user_message` as `kind: "user_message"`.
- Include `agent.pair.system_message` as `kind: "system_message"`.
- The pair window starts at `run.pair.started.seq` and ends at `run.pair.ended.seq` or `run.pair.failed.seq`, inclusive. For active pairs, the window has no upper bound.
- Include `agent.message`, `agent.tool.started`, `agent.tool.completed`, `agent.error`, and `agent.warning` only when the source event sequence falls inside the pair window and its `session_id` matches the pair target.
- Exclude `agent.text.delta`, `agent.reasoning.delta`, and `agent.tool.output.delta`.
- For `agent.tool.started`, use `status: "started"`.
- For `agent.tool.completed`, use `status: "completed"`.
- Tool summaries must be deterministic and compact: include tool name, error status, and a short human-readable preview of arguments/output without embedding full output.
- `next_since_seq` is the highest scanned source event sequence plus `1`, not the last returned transcript entry plus `1`. This lets clients advance past excluded delta/output events without polling the same range forever.

Errors:

- `400` invalid query parameter.
- `404` run not found.
- `404` with `code: "pair_not_found"`.

### `GET /api/v1/runs/{id}/events/{seq}`

Returns one stored run event by source event sequence. Pair transcript `detail_ref.seq` points here.

Query parameters:

- `max_content_length`: integer, minimum `1`, maximum `200000`, default `20000`.

Success `200`: `RunEventDetailResponse`.

```json
{
  "event": {
    "seq": 45,
    "id": "01960d0e-0000-7000-8000-000000000000",
    "ts": "2026-05-18T12:01:20Z",
    "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
    "event": "agent.tool.completed",
    "actor": null,
    "session_id": "ses_01",
    "node_id": "code",
    "node_label": "Code",
    "stage_id": "code@1",
    "tool_call_id": "call_7"
  },
  "properties": {
    "tool_name": "shell",
    "tool_call_id": "call_7",
    "is_error": false,
    "visit": 1
  },
  "content": {
    "kind": "tool_output",
    "value": "..."
  },
  "truncated": false,
  "redacted": false,
  "max_content_length": 20000
}
```

Detail rules:

- Lookup is by `seq` only in v1. Do not add `event_id` lookup until a concrete caller needs it.
- The response must not include an untruncated raw event containing content-bearing `properties.output`, tool arguments, assistant text, or error details.
- Keep event identity and envelope metadata in `event`.
- Keep non-content event properties in `properties`.
- Put content-bearing fields in `content`, with the same truncation and redaction rules applied consistently.
- `truncated` reports whether returned content was shortened because of `max_content_length`.
- `redacted` reports whether secret redaction changed the returned content.

Errors:

- `400` invalid `max_content_length`.
- `404` run not found.
- `404` with `code: "event_not_found"` when the sequence does not exist for the run.

## Pair Lifecycle State Machine

The lifecycle is synchronous at the API/runtime boundary and has no `requested` or `ending` state.

| Current state | Trigger | Next state | Required durable event | API behavior |
| --- | --- | --- | --- | --- |
| none | Runtime confirms `pair.start` for selected target | active | `run.pair.started`, then `agent.pair.system_message { kind: "human_joined" }` | `POST /pair` returns `200` with `status: "active"` |
| none | Target missing, target not pairable, run not pairable, worker unavailable, or runtime rejects start | none | none | `POST /pair` returns the documented `4xx` or `503` |
| active | Runtime accepts paired user message | active | `agent.pair.user_message` | `POST /messages` returns `202` |
| active | Runtime confirms user-requested pair end | ended | `agent.pair.system_message { kind: "human_left" }`, then `run.pair.ended { reason: "user_requested" }` | `DELETE /pair/{pair_id}` returns `200` with `status: "ended"` |
| active | Run ends or target session ends outside explicit pair end | ended | `run.pair.ended` with reason `run_ended` or `session_ended` | `GET /pair/{pair_id}` returns `ended` |
| active | Worker or runtime fails after pair is active | failed | `run.pair.failed` | `GET /pair/{pair_id}` returns `failed` |
| ended | Any lifecycle command | ended | none | mutating commands return `409 pair_not_active` |
| failed | Any lifecycle command | failed | none | mutating commands return `409 pair_not_active` |

Lifecycle rules:

- `POST /pair` must not return success until the runtime has applied pair mode, recorded `run.pair.started`, queued `human_joined`, and requested interruption/parking.
- `DELETE /pair/{pair_id}` must not return success until the runtime has ended pair mode and recorded `run.pair.ended`.
- `human_joined` and `human_left` system messages sit inside the pair window, between `run.pair.started` and `run.pair.ended`.
- Pair read handlers reconstruct state by reading durable pair events in sequence order. They must not write timeout-derived failure events based only on absence of `run.pair.ended`.
- `run.pair.ended` and `run.pair.failed` are mutually exclusive terminal events. Clean termination emits `run.pair.ended`; abnormal termination emits `run.pair.failed`; do not emit both for the same pair.
- Worker-exit handling, run-finalization handling, or runtime error handling records `run.pair.failed` or `run.pair.ended` at the moment that failure/end is observed.
- Repeated end on an already terminal pair returns `409 pair_not_active`, not a second terminal event.

## Event Contract

All events are typed `RunEvent`s. Keep actor, session, node, stage, and tool identity in existing top-level envelope fields.

Canonical envelope fields:

```json
{
  "id": "01960d0c-5d16-7d6e-8f61-9fd6f4a532b5",
  "ts": "2026-05-18T12:00:00Z",
  "run_id": "01HZX6M0P7SE4VJ9Y3X2B8E9QF",
  "event": "run.pair.started",
  "actor": {
    "kind": "user",
    "identity": {
      "issuer": "dev",
      "subject": "alice"
    },
    "login": "alice",
    "auth_method": "dev_token"
  },
  "properties": {}
}
```

### Run-Level Events

`run.pair.started`

Envelope:

- `actor`: user principal that requested start.

Properties:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "target": {
    "stage_id": "code@1",
    "node_id": "code",
    "node_label": "Code",
    "visit": 1,
    "agent_session_id": "ses_01",
    "provider": "openai",
    "model": "gpt-5.3"
  }
}
```

`run.pair.ended`

Envelope:

- `actor`: user principal when user-requested; omitted for system/runtime end.

Properties:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "reason": "user_requested"
}
```

End reasons:

- `user_requested`
- `run_ended`
- `session_ended`

`run.pair.failed`

Envelope:

- `actor`: omitted unless failure is directly tied to a user request that was accepted and then failed while applying.

Properties:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "reason": "worker_gone",
  "message": "The workflow worker exited while pairing was active."
}
```

Failure reasons:

- `worker_gone`
- `runtime_failed`
- `session_failed`
- `run_failed`

### Agent-Target Events

`agent.pair.user_message`

Envelope:

- `session_id`: target API-mode agent session id.
- `node_id`: target node id.
- `node_label`: target node label.
- `stage_id`: target stage id.
- `actor`: user principal that sent the message.

Properties:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "message_id": "01HZX6M4D7Y1QW0Q0P6V8Z4DR5",
  "client_message_id": "optional-correlation-id",
  "text": "Can you inspect the failing test?",
  "visit": 1
}
```

`client_message_id` is omitted when absent.

`agent.pair.system_message`

Envelope:

- `session_id`: target API-mode agent session id.
- `node_id`: target node id.
- `node_label`: target node label.
- `stage_id`: target stage id.

Properties:

```json
{
  "pair_id": "01HZX6M29F1CD5YYMHT1F5D7WQ",
  "kind": "human_joined",
  "text": "A human has joined this workflow run for live pairing. Wait for their next message before continuing.",
  "visit": 1
}
```

Kinds:

- `human_joined`
- `human_left`

Required system message text:

- `human_joined`: `A human has joined this workflow run for live pairing. Wait for their next message before continuing.`
- `human_left`: `The human has ended live pairing. Continue autonomously with the workflow.`

There is no `run.pair.user_message`, `agent.pair.activated`, `agent.pair.deactivated`, or `agent.pair.message.dropped` event in v1. Accepted pair messages must be delivered to the selected active target runtime queue, or the message endpoint must fail.

Existing events that remain unchanged:

- `agent.message`
- `agent.tool.started`
- `agent.tool.completed`
- `agent.error`
- `agent.warning`
- existing steering events

## Implementation Units

### Unit 1: Shared Types And OpenAPI

Files:

- Modify `docs/public/api-reference/fabro-api.yaml`.
- Modify or create `lib/crates/fabro-types/src/pair.rs` and re-export from `lib/crates/fabro-types/src/lib.rs`.
- Modify `lib/crates/fabro-api/build.rs`.
- Add tests under `lib/crates/fabro-api/tests/`.

Steps:

- [ ] Add shared pair schemas and endpoints to OpenAPI with the exact request/response shapes above.
- [ ] Add `PairId`, `PairMessageId`, `PairStatus`, `PairTargetSelector`, `PairTarget`, `PairRecord`, `RunPairStatusResponse`, `PairStartRequest`, `PairMessageRequest`, `PairMessageRecord`, transcript entry types, and `RunEventDetailResponse` to `fabro-types`.
- [ ] Leave `SessionCapability` unchanged. Existing `steer` capability remains the active API-mode control signal that pair target discovery uses.
- [ ] Add `with_replacement(...)` mappings in `fabro-api/build.rs` for shared pair types and the generic event detail response.
- [ ] Run `cargo build -p fabro-api` to regenerate API code.
- [ ] Add type identity and JSON parity tests for pair ids, status enums, selectors, pair records, message records, transcript responses, and event detail responses.

### Unit 2: Typed Pair Events

Files:

- Modify `lib/crates/fabro-types/src/run_event/mod.rs`.
- Modify `lib/crates/fabro-types/src/run_event/agent.rs`.
- Modify `lib/crates/fabro-types/src/run_event/run.rs`.
- Modify `lib/crates/fabro-workflow/src/event/events.rs`.
- Modify `lib/crates/fabro-workflow/src/event/names.rs`.
- Modify `lib/crates/fabro-workflow/src/event/convert.rs`.
- Modify `lib/crates/fabro-workflow/src/event/stored_fields.rs`.

Steps:

- [ ] Add props structs and `EventBody` variants for `run.pair.started`, `run.pair.ended`, `run.pair.failed`, `agent.pair.user_message`, and `agent.pair.system_message`.
- [ ] Add matching internal `Event` variants.
- [ ] Update `event_name()` with exact dot-notation names.
- [ ] Update `event_body_from_event()` with JSON properties matching the event contract.
- [ ] Update `stored_event_fields()` so actor, session, node, stage, and tool identity live in the envelope, not duplicated in properties.
- [ ] Add tests for event names, serialized wire shape, actor lifting, session/stage envelope mapping, and `visit` preservation.

### Unit 3: Runtime Pair Control

Files:

- Modify `lib/crates/fabro-agent/src/session.rs`.
- Modify `lib/crates/fabro-agent/src/types.rs` only if message conversion needs a new internal variant; otherwise keep using existing `Message::User` and `Message::System`.
- Modify `lib/crates/fabro-workflow/src/steering_hub.rs`.
- Modify `lib/crates/fabro-workflow/src/handler/llm/activation_lease.rs`.
- Modify `lib/crates/fabro-workflow/src/handler/llm/api.rs`.

Steps:

- [ ] Replace the text-only steering queue internals with typed control items while preserving existing steering public helpers.
- [ ] Track one active pair per run and one selected target per pair.
- [ ] Pair user messages must append `Message::User` and emit `agent.pair.user_message`.
- [ ] Pair system messages must append `Message::System` and emit `agent.pair.system_message`.
- [ ] Pair start must validate the exact selected target, enable pair mode, record `run.pair.started`, queue `human_joined`, request interruption/parking, and return runtime confirmation.
- [ ] Pair message must record `agent.pair.user_message` once for the selected target after queue acceptance.
- [ ] Pair end must queue `human_left`, disable pair mode, record `run.pair.ended`, and return runtime confirmation.
- [ ] While pair is active, no-tool natural completion must stay parked instead of releasing the stage.
- [ ] After pair end, normal autonomous completion/release behavior must resume.
- [ ] Pair start requires an active API-mode steering-capable target; ACP/CLI-only agents are not pairable.

### Unit 4: Worker Control Transport

Files:

- Modify `lib/crates/fabro-interview/src/control_protocol.rs`.
- Modify `lib/crates/fabro-cli/src/commands/run/runner.rs`.
- Modify `lib/crates/fabro-server/src/server.rs`.

Steps:

- [ ] Add `WorkerControlMessage` variants for `pair.start`, `pair.message`, and `pair.end`.
- [ ] Add constructors on `WorkerControlEnvelope`.
- [ ] Extend worker control so pair start and end return runtime-level confirmed success or typed rejection; mpsc enqueue success alone must not satisfy pair lifecycle API success.
- [ ] Update worker control-line handling to call the workflow pair control methods and send back typed accept/reject results.
- [ ] Extend `RunAnswerTransport` with `start_pair`, `send_pair_message`, and `end_pair`.
- [ ] Preserve existing `steer`, `interrupt`, `interrupt_then_steer`, `answer`, and `cancel` behavior.
- [ ] Add tests proving subprocess transport returns confirmed results for start/end, accepted results for message enqueue, and maps runtime rejections to documented API errors.

### Unit 5: Server API Handlers

Files:

- Add `lib/crates/fabro-server/src/server/handler/pair.rs`.
- Add or modify the existing run event handler for `GET /api/v1/runs/{id}/events/{seq}`.
- Modify `lib/crates/fabro-server/src/server/handler/mod.rs`.
- Modify `lib/crates/fabro-server/src/server.rs`.
- Modify `lib/crates/fabro-client/src/client.rs`.

Steps:

- [ ] Implement pair routes and merge them into the existing server router.
- [ ] Implement `GET /api/v1/runs/{id}/events/{seq}` under the existing run internals/event handler surface.
- [ ] Add live `ManagedRun` state for richer active API target metadata.
- [ ] Implement active pair-eligible target enumeration from live `ManagedRun` state.
- [ ] Implement exact target selection; do not default to all active targets when multiple targets exist.
- [ ] Implement pair lifecycle reconstruction from durable events without read-time timeout reconciliation.
- [ ] Reuse existing run gates: archived rejection, blocked rejection, terminal rejection, and worker-control unavailable handling.
- [ ] Return the documented `ErrorResponse.code` values for all pair conflicts.
- [ ] Add `fabro-client` methods for get pair status, start pair, get pair by id, end pair, send pair message, get transcript, and get run event detail.

### Unit 6: Transcript Projection And Event Detail

Files:

- Add projection helpers in `lib/crates/fabro-server/src/server/handler/pair.rs` or a focused sibling module if the handler becomes too large.
- Add detail helpers in the existing run event handler module.
- Use existing run store event listing APIs; do not add new store schema.

Steps:

- [ ] Reconstruct pair windows from `run.pair.started`, `run.pair.ended`, and `run.pair.failed`.
- [ ] Build compact transcript entries from pair events and matching agent events inside the pair window.
- [ ] Exclude delta events and full tool output.
- [ ] Build deterministic compact tool summaries with `detail_ref.seq`.
- [ ] Compute `next_since_seq` as highest scanned source event sequence plus `1`.
- [ ] Implement run event detail lookup by sequence only.
- [ ] Split run event detail responses into metadata, non-content properties, and truncated/redacted content.
- [ ] Apply `max_content_length` truncation consistently and report `truncated` and `redacted`.

## Test Plan

Run tests test-first for each implementation unit.

Required focused tests:

- `fabro-api`: OpenAPI generation, shared type identity, JSON parity for selectors, pair records/messages/transcript/event detail.
- `fabro-workflow`: event names, event conversion, serialized event shapes, envelope actor/session/stage fields, single-target synchronous pair lifecycle, and message rejection when target cannot accept.
- `fabro-agent`: pair start parks the selected target only, pair message resumes the selected target, no-tool completion remains paired, pair end resumes/release, steering behavior unchanged.
- `fabro-interview` and CLI runner: worker control protocol round trips for `pair.start`, `pair.message`, and `pair.end`, including runtime rejection results.
- `fabro-server`: every endpoint success shape, every documented `409` code, `404` pair/run/event not found, `503` worker unavailable, stale `pair_id`, exact target selection, no broadcast pairing when multiple targets are active, `POST /pair` returns `200 active` only after runtime confirmation, `DELETE /pair` returns `200 ended` only after runtime confirmation, transcript projection, cursor advancement over excluded events, event detail truncation/redaction, and run event persistence.

Required commands:

```sh
cargo build -p fabro-api
cargo nextest run -p fabro-agent -p fabro-workflow -p fabro-server pair
cargo nextest run -p fabro-interview
cargo nextest run -p fabro-server steer interrupt
cargo +nightly-2026-04-14 fmt --check --all
cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings
```

If `cargo nextest run` fails with `Too many open files`, rerun with:

```sh
ulimit -n 4096 && cargo nextest run -p fabro-agent -p fabro-workflow -p fabro-server pair
```

## Assumptions

- V1 pairing is single-target. `POST /pair` requires a target selector and never broadcasts to all active pair-capable targets.
- `POST /pair` and `DELETE /pair/{pair_id}` synchronously confirm lifecycle changes at the runtime boundary and return `200`, not `202`.
- `client_message_id` is correlation-only in v1, not an idempotency guarantee.
- Pair transcript is a server read-side projection over `RunEvent`s.
- Full details use the generic run-event detail endpoint, not a pair-specific endpoint.
- Existing steering APIs and event names remain backward-compatible.
- No MCP, frontend, or `/sessions` removal happens in this slice.
