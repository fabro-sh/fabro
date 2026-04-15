Fixed the remaining drift around canonical `Blocked` status.

What I changed
- Server live model
  - `lib/crates/fabro-server/src/server.rs`
  - Tracks pending interview IDs on managed runs.
  - `InterviewStarted` sets live status to `Blocked`.
  - `InterviewCompleted` / `InterviewTimeout` / `InterviewInterrupted` only return live runs to `Running` when the last pending interview is cleared.
  - Preserves canonical `Removing` instead of remapping it to `Running`.

- Demo API
  - `lib/crates/fabro-server/src/demo/mod.rs`
  - Demo board columns now use `blocked` instead of `pending`.
  - Demo `/runs/{id}` now returns canonical lifecycle statuses and includes `blocked_reason` for blocked demo runs.

- CLI missing-status handling
  - `lib/crates/fabro-cli/src/server_runs.rs`
  - `lib/crates/fabro-cli/src/commands/runs/list.rs`
  - `lib/crates/fabro-cli/src/commands/runs/inspect.rs`
  - `lib/crates/fabro-cli/src/commands/runs/rm.rs`
  - `lib/crates/fabro-cli/src/commands/run/wait.rs`
  - Server summaries no longer coerce missing status to `Failed`.
  - List output renders missing status as `unknown`.
  - Inspect output now emits nullable `status`.
  - Remove refuses unknown-status runs unless `-f` is used.
  - `wait` now treats missing status explicitly: brief startup grace -> `submitted`, otherwise error instead of silently pretending it failed.

- Web terminal status usage
  - `apps/fabro-web/app/routes/run-overview.tsx`
  - Replaced legacy `succeeded` / `dead` exit-node handling with `completed` / `cancelled`.

Validation
- `cargo test -p fabro-cli commands::run::wait -- --nocapture`
- `cargo test -p fabro-cli commands::runs::list -- --nocapture`
- `cargo test -p fabro-server demo_boards_runs_returns_run_list_items -- --nocapture`
- `cargo test -p fabro-server demo_get_run_returns_store_run_summary_shape -- --nocapture`
- `cargo test -p fabro-server boards_runs_maps_statuses_to_columns -- --nocapture`
- `cd apps/fabro-web && bun run typecheck`

Only these 8 files are modified in the working tree.