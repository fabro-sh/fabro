Excellent! Final summary of changes:

**Server (Rust)**:
- `lib/crates/fabro-store/src/slate/run_store.rs` — added `list_events_for_node_from_with_limit` with proper scan-then-filter semantics, plus 5 unit tests
- `lib/crates/fabro-server/src/principal_middleware.rs` — added `RequireRunStageScoped` extractor
- `lib/crates/fabro-server/src/server.rs` — re-exported `RequireRunStageScoped` for handlers
- `lib/crates/fabro-server/src/server/handler/events.rs` — added `list_run_stage_events` handler + route + 6 inline cfg(test) tests
- `lib/crates/fabro-server/src/server/handler/mod.rs` — removed old `/turns` routes; wired demo `/events` route
- `lib/crates/fabro-server/src/demo/mod.rs` — replaced `get_stage_turns` + `runs::turns()` fixture with cursor-paginated `get_stage_events` + 7-event `runs::stage_events()` fixture
- `docs/public/api-reference/fabro-api.yaml` — replaced `/turns` path with `/events`; added `StageNodeId` parameter; deleted `StageTurn`/`SystemStageTurn`/`AssistantStageTurn`/`ToolStageTurn`/`PaginatedStageTurnList`/`ToolUse` schemas

**Server tests**:
- `lib/crates/fabro-server/tests/it/pagination.rs` — removed `listStageTurns` from the offset-pagination matrix
- `lib/crates/fabro-server/tests/it/event_pagination.rs` — new file with 3 cursor-pagination tests for the demo endpoint
- `lib/crates/fabro-server/tests/it/main.rs` — registered new test module

**Frontend (TypeScript)**:
- `lib/packages/fabro-api-client/src/{api,models}/*` — regenerated (StageTurn family removed; `listStageEvents` API added)
- `apps/fabro-web/app/lib/query-keys.ts` — replaced `runs.stageTurns` with `runs.stageEvents`
- `apps/fabro-web/app/lib/api-client.ts` — added `fetchAllStageEvents` cursor-paginated fetcher with empty-page guard
- `apps/fabro-web/app/lib/queries.ts` — replaced `useRunStageTurns`/`useRunEventsList` with `useRunStageEvents`
- `apps/fabro-web/app/lib/run-events.ts` — added `STAGE_ACTIVITY_EVENTS` set; routes activity events to `runs.stageEvents`
- `apps/fabro-web/app/routes/run-stages.tsx` — single-source data flow via `useRunStageEvents` + renamed `eventsToActivity`; deleted `mapApiStageTurn`/`mapTurns`
- `apps/fabro-web/app/lib/query-keys.test.ts` — updated to assert `stageEvents`; added agent-activity invalidation tests
- `apps/fabro-web/app/routes/run-stages.test.ts` — added `eventsToActivity` tests for command/tool pairing and node_id filtering

**Verification**: 79/79 fabro-store tests, 6/6 inline events.rs tests, 3/3 event_pagination IT tests, 553/555 fabro-server tests (the 2 failures are pre-existing graphviz subprocess issues), 97/97 fabro-api tests, 180/180 frontend tests, typecheck clean, fmt clean, clippy clean.