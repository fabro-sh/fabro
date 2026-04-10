---
date: 2026-04-08
topic: fabro-event-schema
focus: greenfield redesign of Fabro's event schemas and streaming contract
---

# Ideation: Fabro Event Schema Redesign

Assumption: greenfield reset. No production deployments, no backward-compatibility constraints, optimize for the best long-term event model rather than incremental migration cost.

These are not ten unrelated features. They are the ten strongest changes to make as one coherent event-platform redesign.

## Codebase Context

- Fabro already has the strongest core envelope in the comparison set: `id`, `ts`, `run_id`, `event`, optional `session_id`, `parent_session_id`, `node_id`, `node_label`, plus `properties`
- `docs-internal/events-strategy.md` already treats events as the durable audit trail that powers storage, SSE, CLI progress, retro analysis, and JSONL sinks
- `RunEvent` and `EventBody` already give Fabro a typed internal model, but the public contract is still more convention-driven than schema-first
- Recent internal plans already push Fabro toward events as source of truth, checkpoint derivation, and simplified `RunEvent`, so the repo is directionally aligned with a stronger event platform
- The biggest remaining gaps are cross-cutting ones: replay, correlation depth, public schema generation, live-stream semantics, and structured status/error/approval states

## Ranked Ideas

### 1. Split Fabro into two event products: a durable event log and a live UI stream
**Description:** Define two first-class streams instead of one overloaded one. The durable log contains immutable domain facts suitable for audit, replay, storage, and projections. The live stream contains UI-oriented deltas, snapshots, keep-alives, and fast-changing progress. They share IDs and correlation fields, but they are different contracts.
**Rationale:** This is the highest-leverage fix. Most competitors get into trouble by mixing audit events, high-frequency streaming deltas, control frames, and reconnect machinery into one schema. Fabro should not. Durable events should be boring and trustworthy. Live events should be optimized for interactivity.
**Downsides:** Two contracts are more work than one. Emitters must decide whether an event is durable, live-only, or both.
**Confidence:** 98%
**Complexity:** High
**Status:** Recommended

### 2. Add a formal replay and resume contract with ordered cursors and snapshot handshakes
**Description:** Every run and session stream gets a monotonic `seq` plus a documented resume protocol: subscribe from cursor, receive the latest snapshot if needed, then apply deltas after `seq > cursor`. Define SSE `id` semantics, dedupe rules, replay buffer guarantees, and failure behavior when a client falls too far behind.
**Rationale:** Goose is the clearest proof that reconnect semantics need to be part of the design, not a side effect. Greenfield Fabro should make "reattach to a long-running workflow" a first-class use case.
**Downsides:** The server needs replay buffers or snapshot storage, and clients need to implement cursor logic correctly.
**Confidence:** 96%
**Complexity:** High
**Status:** Recommended

### 3. Expand the envelope into a real correlation graph
**Description:** Keep the existing strong envelope and add the IDs Fabro will actually want long term: `workflow_id`, `branch_id`, `checkpoint_id`, `turn_id`, `message_id`, `tool_call_id`, `request_id`, `causation_id`, and `correlation_id`. Not every event sets every field, but the contract makes those slots explicit.
**Rationale:** Current Fabro is strong at run/session/node identity, but still thin below that. The next generation of debugging, UI, projection, and analytics work will want to join events by turn, message, tool call, branch, checkpoint, and request without reconstructing those edges from payloads.
**Downsides:** Emitters and handlers must be stricter about ID ownership and propagation.
**Confidence:** 95%
**Complexity:** Medium
**Status:** Recommended

### 4. Make the public event contract schema-first and generated from one registry
**Description:** Keep Fabro's `event` + event-specific payload shape, but stop treating the public wire contract as implicit. Generate JSON Schema, TypeScript types, Rust validators, docs, and streaming examples from one source of truth. Every public event family gets an explicit schema version from day one.
**Rationale:** This is the cleanest fix for drift. Claude Sessions benefits from having a clear public union. OpenCode shows how useful generated event types are. Fabro should combine both while preserving its stronger envelope.
**Downsides:** Codegen and schema governance add process overhead. Some engineers will fight the discipline.
**Confidence:** 94%
**Complexity:** High
**Status:** Recommended

### 5. Normalize lifecycle grammar across all streamable entities
**Description:** Standardize event families around a small lifecycle vocabulary: `.started`, `.delta`, `.snapshot`, `.completed`, `.failed`, `.cancelled`, `.interrupted`. Apply it consistently to runs, stages, sessions, turns, messages, tool calls, commands, checkpoints, parallel branches, and retro work where relevant.
**Rationale:** pi-mono is strongest here. Clients get dramatically simpler when every streamable thing follows the same lifecycle rules instead of bespoke one-off semantics.
**Downsides:** Some event families will feel slightly unnatural if forced into the same lifecycle vocabulary. Discipline matters.
**Confidence:** 93%
**Complexity:** Medium
**Status:** Recommended

### 6. Replace stringly status and failure fields with explicit tagged unions
**Description:** Stop representing terminal and waiting states as loosely-typed strings where possible. Add typed unions for `stop_reason`, `retry_status`, `approval_status`, `wait_reason`, `error_kind`, `failure_class`, and `interrupt_reason`. Preserve display strings separately when useful.
**Rationale:** Claude Sessions gets this exactly right. This is a major quality jump for policy engines, UIs, analytics, and test fixtures. It also reduces accidental schema drift where one code path emits `"timed_out"` and another emits `"timeout"`.
**Downsides:** More up-front schema design. Adding new states later requires more care.
**Confidence:** 96%
**Complexity:** Medium
**Status:** Recommended

### 7. Introduce typed content blocks and block-level deltas for agent output
**Description:** Model agent-facing content as typed blocks instead of mostly strings: `text`, `reasoning`, `tool_call`, `tool_result`, `patch`, `file_ref`, `artifact_ref`, `plan`, `todo`, `command_output`, and `summary`. Live deltas target block IDs rather than appending ambiguous raw text.
**Rationale:** This is the difference between a transcript that only humans can read and one that both humans and tools can reason over. It unlocks richer UIs, selective re-rendering, better retro analysis, and much cleaner summarization and compaction.
**Downsides:** The model is more complex than "event name plus string payload." Poorly chosen block boundaries can make clients awkward.
**Confidence:** 92%
**Complexity:** High
**Status:** Recommended

### 8. Make human-in-the-loop and control-plane semantics first-class events
**Description:** Promote approvals, questions, interrupts, resumes, compactions, and operator interventions into explicit event families: `approval.requested`, `approval.responded`, `question.asked`, `question.answered`, `interrupt.requested`, `interrupt.applied`, `resume.required`, `compaction.started`, `compaction.completed`, `compaction.failed`.
**Rationale:** This is one of the clearest wins from Claude Sessions and OpenCode. Human-in-loop behavior is not edge-case control traffic. It is core workflow state and deserves durable, typed representation.
**Downsides:** It increases surface area. Some flows that are currently implicit must become explicit state machines.
**Confidence:** 94%
**Complexity:** Medium
**Status:** Recommended

### 9. Add first-class snapshot events for fast attach and projection repair
**Description:** Introduce self-contained snapshot events such as `run.snapshot`, `session.snapshot`, and `checkpoint.saved` that intentionally duplicate enough state to let clients and projectors reattach without replaying the full history. Treat these as part of the contract, not ad hoc recovery hacks.
**Rationale:** This complements replay. Durable facts remain append-only, but long-running workflows need efficient recovery points. Greenfield Fabro can design snapshots deliberately instead of letting checkpoint semantics and UI recovery drift apart.
**Downsides:** Snapshot compaction and retention rules must be explicit or the event system becomes harder to reason about.
**Confidence:** 90%
**Complexity:** High
**Status:** Recommended

### 10. Promote model, tool, and command work into first-class span-style event families
**Description:** Treat model requests, tool execution, MCP calls, shell commands, and patch application as first-class event families with started/completed/error plus usage, latency, retries, provider, model, routing, approval outcome, and output references. Do not hide these behind generic stage completion summaries.
**Rationale:** Fabro is an AI workflow product. The event model should expose the actual unit economics and failure surfaces of AI work. This gives better debugging, cost analysis, policy enforcement, and product telemetry than aggregating everything back into stage summaries.
**Downsides:** More event volume. Care is needed to keep live deltas separate from durable summaries.
**Confidence:** 93%
**Complexity:** Medium
**Status:** Recommended

## What This Adds Up To

If Fabro adopted all ten, the resulting idealized model would look like this:

- one durable event log for facts
- one live stream for interactive state
- one shared envelope with strong correlation IDs
- one generated public schema registry
- one consistent lifecycle grammar
- one explicit replay/snapshot story
- typed blocks and typed states instead of strings and ad hoc payloads

That is materially better than any single comparator repo.

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Keep one stream and just document it better | Not enough — durable and live concerns have different optimization goals |
| 2 | Flatten all event-specific fields into the top level | Root churn would make the schema worse, not better |
| 3 | Switch to JSON-RPC-style `method`/`params` notifications | Too transport-shaped for Fabro's broader event-log use case |
| 4 | Use timestamps alone for replay ordering | Weak contract; reconnect needs explicit sequence semantics |
| 5 | Eventize binary artifacts and all large blobs directly | Expensive and noisy; use refs/metadata instead |
| 6 | Keep string errors but standardize message text | Still not machine-readable enough |
| 7 | Make snapshots the only source of truth | Loses auditability and event-sourced advantages |
| 8 | Encode every UI concern in the durable log | Durable logs should stay trustworthy and projection-friendly |
| 9 | Preserve current schema and only add more event names | Misses the deeper contract problems |
| 10 | Treat approvals and questions as transport-level control traffic | These are product-level workflow semantics and belong in the event model |

## Session Log

- 2026-04-08: Grounded ideation from current Fabro event docs and code, plus comparison against Claude Sessions, Claude Code, Goose, OpenAI Codex, OpenCode, and pi-mono. Survivors intentionally optimized for greenfield quality rather than migration ease.
