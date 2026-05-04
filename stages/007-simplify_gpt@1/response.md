Implemented cleanup fixes from the review pass:

- Fixed a `SteeringHub::deliver` race: pending-buffer insertion now stays serialized under the active-session read lock, so a concurrent session registration can’t miss a steer.
- Preserved `agent.steer.dropped` emission for queue overflow even when the dropped steer has no actor.
- Added explicit drain+flush on workflow finalize errors so buffered steers aren’t silently lost on that error path.
- Wired missing run-detail web support:
  - Added `Steer` button for running runs.
  - Mounted `SteerComposer`.
  - Added `useRunToasts` hook for steering SSE toast messages.
- Improved shared SSE infrastructure so multiple subscribers on the same run stream all receive payload callbacks without opening duplicate EventSources or duplicating invalidations.
- Added a focused run-events test covering shared-source payload callbacks and deduped invalidations.

Note: the subagent review tool failed twice with API persistence errors, so I completed the review and cleanup locally.

Validation run:

- `cargo check -q -p fabro-workflow`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo +nightly-2026-04-14 clippy -q -p fabro-workflow --all-targets -- -D warnings`
- `cargo test -q -p fabro-workflow steering_hub`
- `cd apps/fabro-web && bun run typecheck`
- `cd apps/fabro-web && bun test app/lib/run-events.test.tsx app/routes/run-detail.test.ts`