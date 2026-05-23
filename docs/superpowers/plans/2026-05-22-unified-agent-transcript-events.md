# Unified Agent Transcript Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Make the ordered Fabro event stream sufficient to recreate an API-mode agent session without adding a parallel transcript event family.

**Architecture:** Extend existing `agent.message`, `agent.tool.started`, and `agent.tool.completed` event semantics. Messages are communication (`system`, `user`, `reasoning`, `agent`). Tool calls and tool results are actions, not messages. Persist only committed events; partial stream deltas, retries, and interrupted output are not replay sources.

**Out of scope:** Request metadata, compaction semantics, and broad store refactors.

---

## Key Decisions

- Use one shared Fabro transcript model in `fabro-types`; do not create parallel DTOs for events, API, store projection, and runtime history.
- Treat reasoning as a first-class message kind, not a tool call and not part of the visible agent answer.
- Keep model-role semantics (`kind`) separate from audit/source semantics (`source`).
- Keep tool calls/results as enriched action lifecycle records.
- Use event `seq` as the ordering source of truth.
- Keep run/session lifecycle events for lifecycle only; transcript replay comes from `agent.message` and `agent.tool.*`.
- Preserve provider replay payloads as structured parts, not strings.

## Type Ownership

Promote provider-neutral replay primitives from `fabro-llm` into `fabro-types`, then make `fabro-llm` import or re-export the canonical types.

Canonical shared types:

- `ContentPart`
- `ThinkingData`
- `ToolCall`
- `ToolResult`
- `TranscriptMessage`
- `MessageKind`
- `MessageSource`
- `PairMessageRef`
- existing `Principal` for actor attribution

Name the durable transcript type `TranscriptMessage`, not bare `Message`, to avoid import ambiguity with `fabro_agent::Message` and `fabro_llm::types::Message`. Do not add `AgentTranscriptPart` as a second `{ kind, data }` model if `ContentPart` can own the role. Event props must embed the canonical `ToolCall`, `ToolResult`, and `ContentPart` types directly. OpenAPI replacements should point generated API types at these canonical Rust types and include type identity / JSON parity tests.

## Interface Changes

Add shared transcript types in `fabro-types`:

```rust
TranscriptMessage {
    id,
    turn_id,
    kind,   // system | user | reasoning | agent
    source, // system_prompt | turn_input | followup | steer | pair | injected_system | injected_user | loop_detection
    actor: Option<Principal>,
    pair: Option<PairMessageRef>,
    content: Vec<ContentPart>,
    provider,
    model,
    response_id,
    usage,
}

PairMessageRef {
    pair_id,
    message_id,
    client_message_id,
}
```

`kind` captures provider/model-role semantics for replay. `source` captures audit/UI origin. Steering is a source, not a role: steering that currently replays to the LLM as user-role input must be stored as `kind=user, source=steer`.

Extend existing durable events:

- `agent.message`
  - Add `message: TranscriptMessage`.
  - This becomes the canonical replay source for committed system, user, reasoning, and agent messages.
  - Keep narrow `text`, `model`, `billing`, and `tool_call_count` fields until web/server/client consumers are migrated.
- `agent.tool.started`
  - Add `tool_call: ToolCall`.
  - Add `turn_id` and `parent_message_id`.
  - Keep narrow `tool_name`, `tool_call_id`, and `arguments` fields until consumers are migrated.
- `agent.tool.completed`
  - Add `tool_result: ToolResult`.
  - Add `turn_id`.
  - Keep narrow `tool_name`, `tool_call_id`, `output`, and `is_error` fields until consumers are migrated.

Provider replay requirements:

- OpenAI `openai_reasoning` and `openai_message` opaque items remain exact `ContentPart::Other` payloads.
- Anthropic thinking and redacted thinking remain `ContentPart::Thinking` payloads with signatures preserved.
- Gemini `thoughtSignature` remains `ToolCall.provider_metadata`.
- Reasoning messages can contain cleartext, redacted, signed, encrypted, or opaque provider parts, but implementation must not collapse these into plain strings.

Identity requirements:

- Add a canonical `MessageId` in `fabro-types`.
- `fabro-agent::Session` mints a `TurnId` for every `run_single_input()` invocation unless the caller supplies one.
- Ask Fabro passes its existing API `TurnId` into the agent session before processing.
- Workflow API-mode stages let the agent session mint a `TurnId`.
- The assistant/agent message id is minted before emitting tool calls. Tool calls emitted from that response use `parent_message_id = agent_message.id`.

## Implementation Tasks

### 1. Add Typed Event Contracts

Modify:

- `lib/crates/fabro-types/src/run_event/agent.rs`
- `lib/crates/fabro-types/src/run_event/session.rs`
- `lib/crates/fabro-types/src/run_event/mod.rs`
- `docs/public/api-reference/fabro-api.yaml` if exposed wire shapes change

Tasks:

- Move or re-home provider-neutral `ContentPart`, `ThinkingData`, `ToolCall`, and `ToolResult` into `fabro-types`.
- Add canonical `TranscriptMessage`, `MessageKind`, `MessageSource`, and `PairMessageRef` types in `fabro-types`.
- Extend `AgentMessageProps` to carry the canonical message payload.
- Extend tool started/completed props to carry canonical tool call/result payloads plus turn/message linkage.
- Keep serde defaults where needed so old event payloads continue to deserialize.
- Add `fabro-api` replacement tests for type identity and JSON parity when OpenAPI schemas map to canonical Rust types.

### 2. Emit Committed Messages From `fabro-agent`

Modify:

- `lib/crates/fabro-agent/src/types.rs`
- `lib/crates/fabro-agent/src/session.rs`
- `lib/crates/fabro-agent/src/history.rs`

Tasks:

- Replace or extend the narrow assistant-only `AgentEvent::AssistantMessage` path with a general committed `AgentEvent::Message`.
- Emit `kind=system, source=system_prompt` after the exact rendered system prompt is assembled.
- Emit `kind=user, source=turn_input` after skill expansion/wrapping, using the exact user message sent to the model.
- Emit `kind=user, source=followup` for follow-up inputs.
- Emit `kind=user, source=steer` for steering-as-user.
- Emit `kind=user, source=loop_detection` for loop-detection steering.
- Emit `kind=system, source=injected_system` for injected system messages.
- Emit `kind=user, source=injected_user` for injected user-role messages.
- Emit `kind=user, source=pair` for pair chat messages that enter LLM history, with `pair` populated.
- Emit `kind=system, source=pair` for pair join/leave or other pair system messages that enter LLM history, with `pair` populated.
- Emit `kind=reasoning` only for completed provider reasoning blocks that must be preserved for replay, preserving exact structured parts.
- Emit `kind=agent` after provider `Finish`, using the completed response content.
- Do not emit committed messages for deltas, retries, or interrupted partial output.
- Ensure all message events carry `turn_id`, `source`, and optional `actor`/`pair` metadata where applicable.

### 3. Enrich Tool Action Events

Modify:

- `lib/crates/fabro-agent/src/session.rs`
- `lib/crates/fabro-agent/src/tool_execution.rs`
- provider adapters only where extra metadata is not currently surfaced

Tasks:

- Preserve `ToolCall.tool_type`, `raw_arguments`, and `provider_metadata`.
- Preserve `ToolResult` structured output, error state, and supported media/artifact fields.
- Link every tool call to the owning agent message with `parent_message_id`.
- Mint the agent message id before tool execution so tool events can link correctly.
- Keep tool calls/results out of message events.

### 4. Persist Unified Events In Both API Paths

Modify:

- `lib/crates/fabro-workflow/src/handler/llm/api.rs`
- `lib/crates/fabro-workflow/src/event/convert.rs`
- `lib/crates/fabro-workflow/src/event/names.rs`
- `lib/crates/fabro-server/src/server/handler/sessions.rs`

Tasks:

- Convert the unified agent message event through the existing workflow `Event::Agent` path.
- Convert Ask Fabro/server session agent events into the same durable `agent.message` and `agent.tool.*` shapes.
- Keep `run.session.created`, `run.session.turn.started`, and terminal turn events as lifecycle events.
- Keep old `run.session.user_message`, `run.session.assistant_message`, and `run.session.tool_call.*` projection support until all producers and consumers are migrated.
- Prefer a shared event persistence helper for workflow and server session paths so redaction behavior is consistent.
- Avoid creating new transcript-specific event families.

Migration order:

1. Add canonical types and event deserialization support.
2. Update projection to read both old narrow run-session events and new unified agent events.
3. Switch workflow and Ask Fabro producers to emit unified events while retaining compatibility fields.
4. Update web/server/client consumers to prefer unified payloads with narrow-field fallback.
5. Only then consider deprecating narrow transcript-bearing run-session events.

### 5. Define Pair Transcript Relationship

Modify:

- `lib/crates/fabro-workflow/src/steering_hub.rs`
- `lib/crates/fabro-types/src/pair.rs`
- `lib/crates/fabro-server/src/server/handler/sessions.rs`
- web consumers of pair transcript events

Tasks:

- Treat `agent.pair.user_message` and `agent.pair.system_message` as UI/audit projection events only.
- Do not use pair transcript events as replay-authoritative session history.
- For any pair message that affects LLM history, emit the corresponding canonical `agent.message` event with `source=pair` and a populated `PairMessageRef`.
- Store pair user chat as `kind=user, source=pair`.
- Store pair join/leave or other pair system items that enter model context as `kind=system, source=pair`.
- Keep existing pair API transcript types as projections over pair events and canonical message references, not as a second replay model.

### 6. Rebuild Session Projection From Events

Modify:

- `lib/crates/fabro-store/src/run_sessions.rs`
- `lib/crates/fabro-types/src/session.rs`
- `lib/crates/fabro-agent/src/history.rs`

Tasks:

- Project runtime context from ordered `agent.message` and `agent.tool.*` events scoped by envelope `session_id`.
- Add a session-id/sequence index or incremental per-session transcript projection before relying on replay for hydration. Do not scan the full run event history and inspect every event payload for each session load.
- Preserve provider-specific reasoning, opaque provider items, response ids, usage, and tool metadata.
- Keep best-effort fallback projection for legacy narrow session events.
- Ignore pair transcript events for replay except as a legacy fallback path; canonical `agent.message` with `source=pair` is the replay source.
- Ensure `Session::from_record()` can hydrate without dropping provider parts needed for same-provider replay.
- Preserve injected history sources: rendered system prompt, wrapped user input, follow-up input, steering, injected system messages, injected user-role messages, and loop-detection steering.

### 7. Redaction And Security Policy

Modify:

- workflow event persistence path
- server session event persistence path
- event redaction utilities

Tasks:

- Define raw replay fields explicitly: provider opaque parts, raw tool arguments, provider metadata, and structured tool outputs.
- Apply one shared redaction policy before durable storage for both workflow and server sessions.
- Preserve replay-critical opaque provider fields unless they match an existing secret redaction rule.
- Do not omit fields needed for same-provider replay silently; if a field must be redacted, preserve the shape and mark the value redacted.
- Add tests covering raw tool arguments and provider metadata through both persistence paths.

### 8. Consumer Compatibility

Modify:

- web event consumers that currently read narrow `properties.text`
- web pair transcript consumers that read `agent.pair.*`
- server/API projections that expose session detail or event detail
- generated clients if OpenAPI changes

Tasks:

- Keep narrow compatibility fields in emitted events until consumers are updated.
- Update consumers to prefer `properties.message` and fall back to narrow fields.
- Keep pair transcript rendering backed by pair projection events, while ensuring session replay and hydration consume canonical `agent.message` events.
- Add web/server tests that render both old and new event shapes.
- Document the deprecation path for narrow transcript fields after consumer migration.

## Test Plan

- `fabro-types`: serde round trips for `agent.message`, enriched `agent.tool.started`, and enriched `agent.tool.completed`.
- Type ownership:
  - canonical `ToolCall`, `ToolResult`, `ContentPart`, `TranscriptMessage`, usage, and event prop types are reused rather than duplicated
  - OpenAPI replacement tests prove type identity and JSON parity where API schemas expose these shapes
- `fabro-agent`: committed system/user/reasoning/agent messages emit once, while partial deltas and interrupted streams do not create committed messages.
- `fabro-agent`: followups, steering-as-user, injected system messages, and loop-detection steering emit committed messages with the correct `kind`, `source`, and `turn_id`.
- Role/source mapping:
  - steering-as-user emits `kind=user, source=steer`
  - loop-detection steering emits `kind=user, source=loop_detection`
  - injected user-role messages emit `kind=user, source=injected_user`
  - pair user chat emits `kind=user, source=pair` with `PairMessageRef`
  - pair join/leave context emits `kind=system, source=pair` with `PairMessageRef`
- Identity/linkage: tool calls include the parent agent message id minted before tool execution.
- Provider replay:
  - OpenAI encrypted reasoning and opaque message items survive event replay.
  - Anthropic thinking signatures survive event replay.
  - Gemini thought signatures survive enriched tool call replay.
- `fabro-store`: session projection from event `seq` order recreates runtime history including provider parts and tool metadata.
- Pair projection: pair transcript events render in the pair UI/audit surface but do not create duplicate replay history when the canonical `source=pair` message exists.
- Migration: old narrow run-session events and new unified events both hydrate session detail without duplicate transcript entries.
- `fabro-server`: Ask Fabro stores the wrapped model input, not only the raw UI question.
- Redaction: workflow and server session persistence apply the same redaction behavior to raw arguments, provider metadata, and tool outputs.
- Consumer compatibility: existing UI/server consumers render old narrow fields and new unified message payloads.
- API conformance: OpenAPI-generated Rust/TypeScript clients still match the spec after schema updates.

## Acceptance Criteria

- A completed API-mode session can be reconstructed from the event stream without losing committed system, user, reasoning, agent, tool call, or tool result state.
- New transcript state is stored through existing semantic events, not a separate transcript event family.
- Tool calls remain actions, not messages.
- Partial output remains non-authoritative for replay.
- The implementation introduces one canonical set of replay types, not duplicated event/API/runtime DTOs.
- Steering, pair, and injected inputs preserve provider-role semantics in `kind` and audit/source semantics in `source`.
- Pair transcript events are UI/audit projection events, not a replay-authoritative transcript source.
- Ask Fabro migration is backward compatible for existing session events and projections.
