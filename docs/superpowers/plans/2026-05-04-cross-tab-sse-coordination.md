# Cross-Tab SSE Coordination Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Coordinate Fabro web SSE subscriptions across tabs so one browser profile/origin opens at most one UI-owned `/api/v1/attach` EventSource in steady state when `BroadcastChannel` is available; brief overlap during election/takeover is tolerated and deduped.

**Architecture:** Add a browser-side SSE coordinator that elects one tab as leader, has that leader own the global EventSource, and broadcasts parsed run events to follower tabs over `BroadcastChannel`. Existing board and run-detail invalidation logic becomes a consumer of that global event feed, with the current per-tab SSE behavior preserved as a compatibility fallback.

**Tech Stack:** React, SWR, browser `BroadcastChannel`, browser `EventSource`, Bun tests, existing Fabro web API query keys.

---

## Summary

Build a browser-side SSE coordinator so Fabro web opens at most one `/api/v1/attach` EventSource per origin/browser profile in steady state when `BroadcastChannel` is available. Temporary duplicate leaders may exist during election/takeover, but event dedupe and generation checks make the overlap harmless and short-lived. The global stream becomes a shared cache-invalidation feed for both the runs board and run detail pages. No server API, OpenAPI, or Rust streaming contract changes are part of v1.

This supersedes the earlier web SSE limitation documented in `docs/plans/2026-04-19-002-feat-web-ui-lifecycle-actions-plan.md`: the old shared hook was code reuse only; this plan adds actual socket deduplication.

## Implementation Changes

- Add `apps/fabro-web/app/lib/cross-tab-sse.ts`.
  - Export `subscribeToCrossTabSse(...)` with the same invalidation style as `subscribeToSharedEventSource`, a `resyncKeys` callback for gap recovery, and a `fallbackSubscribe` callback used when cross-tab coordination is unavailable.
  - Use `BroadcastChannel` name `fabro:sse:v1`.
  - Generate `tabId` with `crypto.randomUUID()` and a safe random fallback.
  - Open one leader-owned `EventSource` to `queryKeys.system.attach()` (`/api/v1/attach`) in steady state.
  - Leader dispatches each parsed `EventEnvelope` locally and broadcasts it to follower tabs.
  - Followers do not open EventSource while a valid visible leader heartbeat exists; lower lexical `tabId` does not preempt a healthy visible leader.

- Implement leader election in the cross-tab module.
  - Constants: `HEARTBEAT_MS = 1000`, `LEADER_STALE_MS = 4000`, `ELECTION_JITTER_MS = 150`.
  - Messages: `hello`, `heartbeat`, `candidate`, `leader-changed`, `release`, `resync`, `event`.
  - Define a typed message union. Every message includes `type`, `version: 1`, `tabId`, and `sentAt`.
  - `heartbeat`: `{ type, version, tabId, sentAt, leaderId, generation, visibility }`.
  - `candidate`: `{ type, version, tabId, sentAt, candidateId: tabId, candidateGeneration, visibility, observedLeaderId, observedGeneration, reason }`, where `reason` is `"hidden-leader" | "stale-leader" | "release" | "no-leader"`.
  - `leader-changed`: `{ type, version, tabId, sentAt, leaderId, generation, visibility }`.
  - `release`: `{ type, version, tabId, sentAt, leaderId, generation }`.
  - `resync`: `{ type, version, tabId, sentAt, leaderId, generation, reason }`.
  - `event`: `{ type, version, tabId, sentAt, leaderId, generation, payload }`.
  - Use the candidacy phase for all leadership changes: hidden-leader takeover, stale-leader recovery, leader release, and no-leader startup.
  - A candidate sets `candidateGeneration = observedGeneration + 1`, broadcasts `candidate`, waits jitter, and opens EventSource only if no higher-priority candidate for the same `candidateGeneration` appears.
  - Candidate priority is election-scoped: visible candidates outrank hidden candidates; for equal visibility, lower lexical `candidateId` wins. This priority resolves elections and same-generation split brain only; it is not a reason to preempt a fresh visible leader.
  - When a visible follower observes a fresh hidden leader heartbeat, it enters candidacy with `reason: "hidden-leader"`.
  - When tabs detect a stale leader, leader release, or no known leader, they enter the same candidacy flow with the matching `reason`.
  - If two visible candidates race for the same observed leader/generation, the lower lexical `candidateId` wins.
  - Current leaders release when they observe a candidate whose `observedLeaderId` matches their `leaderId` and whose `observedGeneration` is current or newer.
  - If same-generation split brain still occurs, lower-priority leaders release when they observe a same-generation higher-priority leader heartbeat or `leader-changed`.
  - Each new leader uses `candidateGeneration`, broadcasts `leader-changed`, and followers ignore heartbeats/events from non-current leaders or stale generations.
  - Hidden leader keeps the stream only when no visible candidate takes over.
  - On `pagehide`/last local unsubscribe, a leader closes EventSource and broadcasts `release`.
  - Brief split brain is tolerated; dedupe events by `payload.id`, falling back to `${run_id}:${seq}:${event}`.
  - Keep dedupe bounded with a recent-event cache: max 1000 IDs and 5 minute TTL. Evict oldest entries when the max is exceeded and prune expired entries during event handling. Duplicate invalidations after eviction are acceptable; unbounded growth is not.

- Migrate consumers.
  - `apps/fabro-web/app/lib/board-events.ts`: subscribe through the cross-tab global stream; keep existing board event allowlist.
  - `apps/fabro-web/app/lib/run-events.ts`: subscribe through the same global stream, filter by `payload.run_id === runId`, and reuse `queryKeysForRunEvent`.
  - In coordinated mode, run detail pages stay subscribed while mounted, including terminal runs, so post-terminal archive/unarchive changes can reconcile live.
  - Do not close the global stream on `run.completed` / `run.failed`; terminal events only invalidate run-scoped keys.
  - Keep `subscribeToSharedEventSource` in `apps/fabro-web/app/lib/sse.ts` for fallback and existing local sharing behavior.

- Gap and fallback behavior.
  - If `BroadcastChannel` is unavailable or throws, call each subscriber's `fallbackSubscribe`.
  - Board fallback uses the existing global `/api/v1/attach` path.
  - Run detail fallback preserves the existing run-scoped `/api/v1/runs/:id/attach` path, so the old terminal-tab stale limitation remains only in fallback mode.
  - Do not add replay to `/api/v1/attach`.
  - On leader takeover, stale leader timeout, leader release, and new leader generation, broadcast `resync` or `leader-changed` so every tab with active local subscriptions runs its own `resyncKeys`.
  - On `visibilitychange` back to visible without leadership change, run only that tab's local `resyncKeys`; do not broadcast cross-tab resync.
  - Board `resyncKeys`: `queryKeys.boards.runs()`.
  - Run `resyncKeys`: detail, files, billing, stages, events, LR graph, TB graph, and questions for that run.

## Tests

- Add `apps/fabro-web/app/lib/cross-tab-sse.test.ts` with fake `BroadcastChannel`, fake `EventSource`, and fake timers.
  - One leader opens `/api/v1/attach`; followers open no EventSource.
  - Leader broadcasts an event and all local subscribers receive invalidations.
  - Run subscribers ignore events for other `run_id` values.
  - Board and run subscriptions can coexist on the same global stream.
  - Temporary duplicate leaders are allowed only during election/takeover and converge back to one leader.
  - With fresh hidden-leader heartbeats, a visible tab broadcasts candidacy, hidden leader closes, visible tab opens `/api/v1/attach`, and followers resync.
  - Two visible candidates racing for the same hidden leader resolve to the lower lexical `candidateId`.
  - Two tabs detect the same stale leader simultaneously; only the winning candidate opens `/api/v1/attach` after jitter.
  - Same-generation split brain converges to one leader by visibility, then lexical `tabId`.
  - A fresh visible leader exists; a new visible follower with a lower lexical `tabId` joins and does not take leadership.
  - A hidden leader does not release for a candidate that references an old `observedLeaderId` or stale `observedGeneration`.
  - Stale heartbeat triggers takeover and every active tab calls its own `resyncKeys`.
  - A follower tab calls its own board/run `resyncKeys` after another tab becomes leader.
  - Duplicate event IDs are ignored.
  - The recent-event dedupe cache evicts by TTL and max-size bound; duplicate invalidation may recur only after eviction.
  - Stale heartbeat/event messages from an old leader/generation are ignored after takeover.
  - Last unsubscribe closes leader EventSource and sends `release`.
  - Missing/broken BroadcastChannel uses the per-tab fallback.

- Update existing tests.
  - `apps/fabro-web/app/lib/board-events.test.tsx`: assert coordinated mode uses `/api/v1/attach` once and fallback preserves current behavior.
  - `apps/fabro-web/app/lib/run-events.test.tsx`: assert coordinated mode filters by `run_id`; fallback preserves current `/runs/:id/attach` behavior; terminal run events do not close the global coordinator; terminal detail tabs still receive archive/unarchive invalidations while mounted in coordinated mode.

- Verification commands.
  - `cd apps/fabro-web && bun test app/lib/cross-tab-sse.test.ts app/lib/board-events.test.tsx app/lib/run-events.test.tsx`
  - `cd apps/fabro-web && bun run typecheck`
  - Optional manual check: open 8 run-detail tabs for active runs; after election settles and with BroadcastChannel available, Chrome should show one active `/api/v1/attach` EventStream across the participating tabs and no UI-created `/api/v1/runs/:id/attach` streams.

## Assumptions

- Treat SSE as live cache invalidation, not an exact gapless event log.
- Preserve current run-specific attach endpoint for CLI, API clients, and fallback only.
- Do not touch unrelated dirty worktree files; intentionally replace or adapt the earlier interrupted SSE test edits as part of the new test suite.
- No docs or public API updates are required for v1 because this is an internal web transport change.
