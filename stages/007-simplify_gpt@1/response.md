Reviewed the stage URL changes and applied cleanup fixes.

Note: I launched the three parallel review agents twice, but the subagent tool failed on `wait` both times with an LLM item persistence error. I closed those agents and completed the reuse/quality/efficiency review locally.

Fixed:
- Restored `stageIdFromPayload` fallback to `node_id` / `properties.node_id` while still preferring full `stage_id`.
- Added a fallback invalidation test for node-id-only SSE payloads.
- Treated `retrying` as an active state in the stage detail header, so the icon spins and duration ticks like `running`.
- Added assertions for sidebar display labels (`Apply Changes`, `Apply Changes (2)`).
- Simplified a no-op async/cancellation wrapper in the graph SVG annotation effect.
- Cleaned Rust type imports to use named types (`StageProjection`, `StageId`) instead of fully-qualified type paths.
- Applied rustfmt’s suggested function signature formatting.
- Ensured changed text files end with newlines.

Validation passed:
- `cargo check -p fabro-server -p fabro-workflow`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo nextest run -p fabro-server list_run_stages_distinguishes_visits list_run_stages_shows_retrying_after_failed_event list_run_stages_shows_retrying_when_failed_will_retry list_run_stages_projects_retrying_until_completion`
- `cd apps/fabro-web && bun test app/lib/run-events.test.tsx app/lib/stage-sidebar.test.ts app/routes/run-stages.test.ts`
- `cd apps/fabro-web && bun run typecheck`
- `git diff --check`