# Event Schema Competitive Analysis

Date: 2026-04-08

This report compares the event schemas used by:

- Claude Sessions API
- Claude Code
- Goose
- OpenAI Codex
- OpenCode
- pi-mono

Goal: identify patterns Fabro should copy, avoid, or formalize more clearly.

## Executive Summary

Fabro's current event model is already ahead of most of the field on one important point: it has a canonical envelope with stable metadata (`id`, `ts`, `run_id`, `event`, optional `session_id`, `parent_session_id`, `node_id`, `node_label`) and a typed internal-to-external mapping.

The biggest improvement opportunities are not "more events." They are:

1. Keep transport concerns separate from domain events, but document them as part of the contract.
2. Make every streamed event part of one explicit public schema. Avoid opaque blobs and server-injected fields that the schema does not admit.
3. Add more first-class correlation fields where the UI or downstream systems need them, especially `turn_id`, `message_id`, `tool_call_id`, `request_id`, and retry/attempt IDs.
4. Make retry, stop, idle, and requires-action states machine-readable unions instead of loose strings.
5. Be explicit about delta vs snapshot semantics and replay behavior.

## Fabro Baseline

Fabro's current strategy is documented in `docs-internal/events-strategy.md`. The canonical external shape is:

```json
{
  "id": "uuidv7",
  "ts": "2026-03-30T12:00:01.000Z",
  "run_id": "01JQ...",
  "event": "agent.tool.started",
  "session_id": "ses_child",
  "parent_session_id": "ses_parent",
  "node_id": "code",
  "node_label": "Code",
  "properties": { "...": "..." }
}
```

That envelope is stronger than most comparator systems. It gives Fabro stable top-level metadata, keeps event-specific data inside `properties`, and avoids flattening arbitrary fields into the root.

Relevant current Fabro sources:

- `docs-internal/events-strategy.md`
- `lib/crates/fabro-workflow/src/event.rs`
- `lib/crates/fabro-types/src/run_event/mod.rs`
- `lib/crates/fabro-agent/src/types.rs`

## Comparison Matrix

| System | Public event surface | Discriminator | Universal envelope fields on every event | Replay / ordering story | Main strength | Main weakness |
| --- | --- | --- | --- | --- | --- | --- |
| Claude Sessions | 20-event public union | `type` | `id`, `processed_at` | Event IDs exist; replay semantics are not part of the event payload | Very explicit, stable union with typed nested states | Less transport detail and fewer workflow-specific events |
| Claude Code | 24 core SDK messages, 31 stdout/control variants | `type`, often `subtype` | Usually `uuid`, `session_id`; no universal timestamp | Streaming is batched/coalesced; control and domain share the same channel | Rich task, hook, status, and tool progress | Nested stream payload is opaque at runtime; control and data are mixed |
| Goose | 7 `MessageEvent` variants | `type` | None in JSON payload; SSE `id` is outside payload | Strong SSE replay via monotonic seq + `Last-Event-ID` + replay buffer | Reattach/replay semantics are clear | Transport fields are injected outside schema; payload typing is shallow |
| Codex SDK | 8 thread events | `type` | No universal ID/timestamp | Ordered stream, no replay contract in payload | Very simple client model | Too minimal for rich UIs and analytics |
| Codex app-server | 49 notification methods | `method` + `params` | No universal ID/timestamp in params | Ordered notifications, no seq/replay field | Richest low-level protocol in the set | Fragmented event story; transport shape leaks into the schema |
| OpenCode | 45 generated event variants | `type` + `properties` | No universal ID/timestamp | Plain SSE; client supports SSE IDs but server does not emit them | Broadest app/runtime event coverage | Wire/schema drift and no universal envelope |
| pi-mono | 12 assistant stream events, 10 agent events, 14 session events | `type` | Session header only; not per event | JSONL stream, no replay contract | Excellent streaming lifecycle grammar | No durable universal envelope for downstream consumers |

## System Notes

### Claude Sessions

What it does well:

- One explicit public union.
- Every event has `id`, `type`, and `processed_at`.
- Tool confirmation, custom tool results, MCP tool use, session errors, session status, and model-span events are all first-class.
- Terminal and waiting states are structured. `session.status_idle.stop_reason` is not a loose string; it is a small union.
- Error reporting is structured. `session.error.error` is a tagged union, not just a message.

Why it matters for Fabro:

- This is the cleanest example of a public agent-session event API that is still small enough to understand.
- The main idea to copy is not the exact event list. It is the discipline: explicit tagged unions for stop reasons, errors, and status transitions.

Sources:

- `https://platform.claude.com/docs/en/api/beta/sessions/events/stream`
- `https://platform.claude.com/docs/specs/merged.53db30dfcc06f431.json.gz`

### Claude Code

Schema shape:

- `SDKMessageSchema` contains 24 core message variants.
- `StdoutMessageSchema` expands the stdout protocol to 31 variants once control messages and keep-alives are included.
- Many events use `type: "system"` plus a `subtype`, for example `init`, `status`, `api_retry`, `hook_started`, `task_progress`, and `session_state_changed`.
- Streaming assistant output is wrapped as `type: "stream_event"`.

What it does well:

- It covers more than just model output: task lifecycle, hook lifecycle, compaction boundaries, retries, authentication state, file persistence, tool progress, prompt suggestions.
- It carries `uuid` and `session_id` widely, which is useful for correlation.
- It includes explicit session-state transitions (`idle`, `running`, `requires_action`).

What is weak:

- The nested streaming payload is not explicitly validated at runtime. `RawMessageStreamEventPlaceholder` is `z.unknown()`.
- Control protocol messages live in the same stream as domain messages.
- `type: "system"` plus `subtype` is workable, but less ergonomic than a flatter public union.
- There is no universal top-level timestamp on every event.

Why it matters for Fabro:

- Copy the breadth, not the shape.
- Avoid opaque inner payloads in public schemas.
- Avoid mixing keep-alive/control/config traffic into the same schema that product consumers use for analytics and UI rendering.

Sources:

- `/Users/bhelmkamp/p/AnkanMisra/claude-code/src/entrypoints/sdk/coreSchemas.ts`
- `/Users/bhelmkamp/p/AnkanMisra/claude-code/src/entrypoints/sdk/controlSchemas.ts`
- `/Users/bhelmkamp/p/AnkanMisra/claude-code/src/remote/sdkMessageAdapter.ts`
- `/Users/bhelmkamp/p/AnkanMisra/claude-code/src/cli/transports/ccrClient.ts`
- `/Users/bhelmkamp/p/AnkanMisra/claude-code/src/utils/sdkEventQueue.ts`

### Goose

Schema shape:

- One small SSE payload union: `Message`, `Error`, `Finish`, `Notification`, `UpdateConversation`, `ActiveRequests`, `Ping`.
- SSE `id:` carries a monotonic sequence number.
- Session replay uses `Last-Event-ID` plus a replay buffer.
- `request_id` and `chat_request_id` are injected at the SSE framing layer, not modeled in the payload type.

What it does well:

- Clear reattach story.
- Monotonic sequence numbers are transport-level, not payload-level.
- `ActiveRequests` lets the client discover in-flight work when reconnecting.

What is weak:

- The public event payload omits fields the client actually consumes.
- `Notification.message` is effectively an untyped object.
- The session stream also emits comment heartbeats outside the schema, and there is a separate `Ping` payload variant in the shared enum. That split is easy to drift.

Why it matters for Fabro:

- Goose is the best example here for replay and reconnect semantics.
- The lesson is not "put sequence numbers in the payload." The lesson is "formalize replay outside the payload, and do not rely on undocumented injected fields."

Sources:

- `/Users/bhelmkamp/p/block/goose/crates/goose-server/src/routes/reply.rs`
- `/Users/bhelmkamp/p/block/goose/crates/goose-server/src/routes/session_events.rs`
- `/Users/bhelmkamp/p/block/goose/crates/goose-server/src/session_event_bus.rs`
- `/Users/bhelmkamp/p/block/goose/ui/desktop/openapi.json`
- `/Users/bhelmkamp/p/block/goose/ui/desktop/src/hooks/useSessionEvents.ts`

### OpenAI Codex

There are really two event systems:

1. The TypeScript SDK `ThreadEvent` surface.
2. The app-server `ServerNotification` protocol.

SDK shape:

- 8 high-level events: thread started, turn started/completed/failed, item started/updated/completed, fatal stream error.
- Rich detail is pushed down into `ThreadItem`, which includes `agent_message`, `reasoning`, `command_execution`, `file_change`, `mcp_tool_call`, `web_search`, `todo_list`, and `error`.

App-server shape:

- 49 server notification methods.
- Notifications are discriminated by `method`, with a typed `params` object.
- Coverage includes thread lifecycle, turn lifecycle, item lifecycle, deltas, token usage, command output, MCP progress, model reroutes, config warnings, and experimental realtime notifications.

What it does well:

- Good separation of a simple developer-facing SDK from a richer system protocol.
- The low-level protocol is broad and explicit.
- Experimental notifications are clearly labeled as experimental.

What is weak:

- The event story is fragmented. "Which schema should I build against?" depends on which integration layer you pick.
- There is no universal timestamp or universal event ID in the event bodies.
- `method` + `params` is transport-shaped. It works well for JSON-RPC, but it is not as clean as a transport-agnostic event envelope.

Why it matters for Fabro:

- If Fabro needs both a high-level SDK and a low-level protocol, document the layering explicitly.
- If Fabro only needs one event stream, a single canonical envelope is simpler than method-shaped notifications.

Sources:

- `/Users/bhelmkamp/p/openai/codex/sdk/typescript/src/events.ts`
- `/Users/bhelmkamp/p/openai/codex/sdk/typescript/src/items.ts`
- `/Users/bhelmkamp/p/openai/codex/sdk/typescript/src/thread.ts`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/schema/typescript/ServerNotification.ts`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/src/protocol/common.rs`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/schema/typescript/v2/TurnStartedNotification.ts`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/schema/typescript/v2/TurnCompletedNotification.ts`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/schema/typescript/v2/AgentMessageDeltaNotification.ts`
- `/Users/bhelmkamp/p/openai/codex/codex-rs/app-server-protocol/schema/typescript/v2/CommandExecOutputDeltaNotification.ts`

### OpenCode

Schema shape:

- One generated `Event` union with 45 variants.
- `GlobalEvent` adds `directory` plus `payload: Event`.
- Events use `type` plus a `properties` object.
- Coverage includes questions, permissions, messages, message parts, session status/idle/compacted/error/diff, workspace readiness, PTYs, worktrees, VCS, file edits, MCP, TUI commands, and more.

What it does well:

- Broad coverage.
- Generated API types from the server surface.
- Clear split between session-scoped events and global events.
- Status is partly structured. `SessionStatus` is a union of `idle`, `retry`, and `busy`.

What is weak:

- No universal event ID.
- No universal timestamp.
- No replay cursor or sequence field.
- The wire stream emits `server.heartbeat`, but the generated `Event` union does not include it.
- The global event schema says `directory` is present, but the initial global `server.connected` and heartbeat frames omit it.

Why it matters for Fabro:

- OpenCode shows how far a generated event surface can go.
- It also shows the cost of not having a canonical envelope: clients must reconstruct correlation from nested `sessionID`, `messageID`, `partID`, and route-specific wrappers.

Sources:

- `/Users/bhelmkamp/p/anomalyco/opencode/packages/sdk/js/src/v2/gen/types.gen.ts`
- `/Users/bhelmkamp/p/anomalyco/opencode/packages/sdk/js/src/v2/gen/core/serverSentEvents.gen.ts`
- `/Users/bhelmkamp/p/anomalyco/opencode/packages/opencode/src/server/server.ts`
- `/Users/bhelmkamp/p/anomalyco/opencode/packages/opencode/src/server/routes/global.ts`
- `/Users/bhelmkamp/p/anomalyco/opencode/packages/opencode/src/server/event.ts`
- `/Users/bhelmkamp/p/anomalyco/opencode/packages/web/src/content/docs/server.mdx`

### pi-mono

Schema shape:

- `AssistantMessageEvent` has 12 streaming variants: `start`, block start/delta/end for text, thinking, and tool calls, then `done` or `error`.
- `AgentEvent` has 10 lifecycle variants across agent, turn, message, and tool execution.
- `AgentSessionEvent` extends `AgentEvent` with 4 session-only retry/compaction events.
- JSON mode starts with a session header, then emits JSONL events.

What it does well:

- Excellent streaming lifecycle grammar.
- Strong layering:
  - low-level assistant stream events
  - mid-level agent lifecycle events
  - high-level session events
- The proxy mode has a bandwidth-optimized streaming shape that intentionally strips partial message snapshots and reconstructs them client-side.

What is weak:

- There is no universal per-event envelope.
- There are no event IDs or replay semantics.
- Timestamping is inconsistent. The session header has a timestamp, and some embedded message objects have timestamps, but not every event line does.

Why it matters for Fabro:

- pi-mono is the best example here for event layering and start/delta/end/done grammar.
- Fabro should borrow that lifecycle discipline if it expands live agent streaming, but keep Fabro's stronger envelope.

Sources:

- `/Users/bhelmkamp/p/badlogic/pi-mono/packages/ai/src/types.ts`
- `/Users/bhelmkamp/p/badlogic/pi-mono/packages/agent/src/types.ts`
- `/Users/bhelmkamp/p/badlogic/pi-mono/packages/agent/src/proxy.ts`
- `/Users/bhelmkamp/p/badlogic/pi-mono/packages/coding-agent/src/core/agent-session.ts`
- `/Users/bhelmkamp/p/badlogic/pi-mono/packages/coding-agent/docs/json.md`

## Cross-System Patterns

### Patterns worth copying

- One obvious discriminator per public event.
- Explicit unions for error state, stop reason, retry state, and requires-action state.
- Stable correlation IDs for session/thread/turn/message/tool levels.
- A documented replay story for long-running streams.
- Generated public schemas from one source of truth.
- A clear distinction between snapshot events and delta events.

### Patterns worth avoiding

- Opaque `unknown` payloads inside otherwise typed events.
- Server-injected fields that the public schema does not model.
- Mixing keep-alives, control RPCs, and domain events in one event contract.
- Event systems that only make sense in the context of one transport, for example JSON-RPC `method`/`params`, when the real need is a transport-agnostic event log.
- No universal ID or timestamp on durable events.

## Recommendations For Fabro

### 1. Keep the canonical envelope

Fabro should keep `id`, `ts`, `run_id`, `event`, `session_id`, `parent_session_id`, `node_id`, and `node_label` exactly as the backbone of the public schema. That is already better than every comparator except Claude Sessions on consistency.

### 2. Do not let transport metadata leak informally

If Fabro supports SSE replay or live reattach, define transport rules explicitly:

- SSE `id`
- replay cursor semantics
- comment heartbeat vs payload heartbeat
- reconnect guarantees

Do not make clients depend on extra fields injected by one server path that are absent from the formal schema.

### 3. Add deeper correlation IDs where the product needs them

Fabro already has run/session/node metadata. The next likely additions are:

- `turn_id`
- `message_id`
- `tool_call_id`
- `request_id`
- `attempt`

Those should be explicit schema fields, not encoded into ad hoc strings.

### 4. Prefer unions over stringly terminal state

If Fabro expands live session or agent events, model terminal and waiting states like Claude Sessions does:

- `stop_reason`
- `retry_status`
- `requires_action`
- `error_kind`

Avoid free-form strings when a small tagged union will do.

### 5. Standardize lifecycle families

If Fabro emits live agent output, choose one lifecycle grammar and document it:

- `.started`
- `.delta`
- `.completed`
- `.failed`

If snapshot replacement events also exist, mark them clearly and document when clients should treat them as authoritative replacement vs append-only updates.

### 6. Keep domain events separate from control and keep-alive traffic

Claude Code shows the downside of multiplexing control requests, control responses, keep-alives, and domain messages in one stream contract. Fabro's durable run events should stay product-facing and analyzable.

### 7. Add schema-drift tests for streamed events

OpenCode and Goose both show how easy it is for the wire stream to diverge from the published schema. Fabro should keep tests that validate:

- every emitted streamed payload is representable by the public schema
- no consumer-visible fields are injected outside the schema
- replay/heartbeat frames are documented and tested separately

## Bottom Line

Fabro does not need to copy any one competitor's schema wholesale.

The best composite design is:

- Claude Sessions' explicit unions for status and errors
- Goose's replay semantics
- Codex's separation between a simple high-level client view and a richer low-level view, if Fabro ever needs both
- OpenCode's breadth of runtime events
- pi-mono's streaming lifecycle grammar
- Fabro's existing canonical envelope as the foundation

That combination would produce an event model that is both durable and ergonomic: good for live UI streaming, replay, analytics, tests, and long-term compatibility.
