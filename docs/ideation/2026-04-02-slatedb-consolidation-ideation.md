---
date: 2026-04-02
topic: slatedb-consolidation
focus: Consolidate SlateDB from one-per-run to single instance owned by fabro server, all access via HTTP/Unix socket
---

# Ideation: SlateDB Consolidation Behind Server

## Codebase Context

**Project shape:** Rust workspace (30 crates) + React 19 frontend. AI-powered workflow orchestration platform.

**Current storage architecture:**
- `fabro-store` crate defines `Store` and `RunStore` async traits with `SlateStore` and `InMemoryStore` impls
- Each workflow run gets its own SlateDB database (unique `db_prefix` per run)
- `SlateRunDb` enum distinguishes Writer (`slatedb::Db`) and Reader (`DbReader`)
- CLI opens SlateDB directly via `build_store()` (~25 call sites)
- Server also opens SlateDB directly
- Unix socket binding and server daemon management already implemented

**Key constraints:**
- SlateDB chosen for "bottomless" S3 storage path — not replaceable with SQLite
- Greenfield app, not yet deployed — no migration concerns
- Events-as-source-of-truth migration actively in progress
- Moving away from SQLite dependency, not toward it

## Ranked Ideas

### 1. Events-First Server Architecture
**Description:** Make events the sole write path. CLI POSTs events to the server, server materializes all state (run record, node outcomes, checkpoints, etc.) from events via projection. SSE pushes events to subscribers instantly. The Writer/Reader distinction is eliminated. The API is coarser-grained REST shaped by domain needs (POST /runs/{id}/events, GET /runs/{id}/state, GET /runs/{id}/events?stream SSE) — not a 1:1 mirror of the 40-method RunStore trait.
**Rationale:** Collapses 40 put methods to a single append endpoint. Converges with the events-as-source-of-truth work already in progress. Makes SSE push, Writer/Reader elimination, trait flattening, and in-memory projection all fall out as natural consequences. Both critics called this "the plan."
**Downsides:** Requires events-as-source-of-truth to be complete enough that all RunStore state is derivable from events. The follow-ups doc lists gaps.
**Confidence:** 90%
**Complexity:** High
**Status:** In Progress

### 2. Auto-Start Server Daemon
**Description:** When a CLI command needs store access and `fabro.sock` is missing (or unreachable), automatically run `fabro server start` before proceeding. Uses existing daemon management and readiness probe.
**Rationale:** Without this, every CLI command fails unless the user manually starts the server first. The daemon infrastructure already exists (start.rs has try_connect readiness probe, daemon spawning, flock locking). Auto-start is the difference between smooth DX and broken DX.
**Downsides:** First command in a session pays ~1-2s startup cost. Edge cases around stale sockets and failed starts.
**Confidence:** 95%
**Complexity:** Low
**Status:** Unexplored

### 3. Thin HTTP Client for CLI
**Description:** An HTTP-backed `Store`/`RunStore` implementation that connects to the server over Unix socket. With events-first (#1), this is thin — mostly `POST /events`, SSE subscription, and a handful of read endpoints. Replaces the ~25 `build_store()` call sites.
**Rationale:** The mechanical bridge between CLI and server. The trait boundary is the right seam. `InMemoryStore` tests are completely unaffected.
**Downsides:** Error handling for server-down scenarios (mitigated by auto-start #2).
**Confidence:** 95%
**Complexity:** Medium
**Status:** Unexplored

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Batch write API | Premature optimization — measure Unix socket latency first |
| 2 | Run-prefixed key namespace | Implementation detail, falls out of engine design |
| 3 | Streaming binary assets | YAGNI — no evidence of multi-MB blob problems |
| 4 | Graceful fallback / offline mode | Fights the architecture — auto-start is the answer |
| 5 | Connection pooling | Default HTTP client behavior, not a decision |
| 6 | In-memory projection cache | Redundant with events-first — materialization IS the cache |
| 7 | Eliminate SlateDB (BTreeMap + WAL) | Building a database; SlateDB provides S3 path |
| 8 | Auto-generate HTTP client from trait | Contradicts coarser-grained API design |
| 9 | Lazy-load sequence counters | Micro-optimization; events-first removes the problem |
| 10 | Kill HTTP; shared SlateDB as IPC | Contradicts architecture; couples to storage internals |
| 11 | Flip dependency (server in CLI) | Architecturally backwards from stated direction |
| 12 | Event log as IPC (no HTTP) | Shared file append is fragile; HTTP serializes for free |
| 13 | Consolidate catalog only | Half-measure creating a third topology |
| 14 | CLI event-streaming client | Subsumed by events-first |
| 15 | Time-travel / run replay | Not a consolidation decision; events-first makes it free later |
| 16 | Multi-workspace fan-in | Feature creep; no current demand |
| 17 | Reactive web UI via events | Orthogonal frontend concern |
| 18 | Persistent run queue | Orthogonal scheduler feature |
| 19 | MCP host for external agents | Orthogonal; fabro-mcp exists |
| 20 | Tombstone soft delete | Implementation detail of engine |
| 21 | Intent log for crash recovery | Redundant — event log IS the intent log |
| 22 | Column families | SlateDB doesn't support them; premature |
| 23 | Optimistic concurrency control | Single server owns writes — no concurrent writer problem |
| 24 | Backpressure / write batching | Premature; no write pressure evidence |
| 25 | SQLite catalog + cross-run queries | Moving away from SQLite dependency; use SlateDB-based index |
| 26 | Migration command | Greenfield app, not yet deployed |
| 27 | Consolidate to SQLite | SlateDB provides bottomless S3 storage |

## Session Log
- 2026-04-02: Initial ideation — 35 candidates generated (6 agents), 6 survived critique, 3 accepted by user (rejected: SQLite catalog, migration command, SQLite as engine)
