# CLI Run Lifecycle Cleanup Compaction

## Summary
Simplify the proven server-backed CLI run lifecycle without changing execution topology in this pass.

This pass keeps `/api/v1/runs/*` as the canonical run API, ignores `/boards/runs`, and treats the current server-owned in-process execution model as the stable behavior for now. The goal is to remove obsolete local-launcher compatibility, collapse duplicated server-backed lookup code, and leave the CLI in a cleaner client-to-server shape. The hidden internal command is renamed from `__detached` to `__runner` now so the code reflects its future role, but this pass does not yet make the server spawn it.

## Key Changes
### 1. Lock the cleanup boundary
- Treat the run lifecycle surface as server-backed and cleanup-eligible:
  - `run`, `create`, `start`, `resume`, `attach`, `wait`, `logs`, `diff`, `preview`, `ssh`, `runs/*`, `pr/*`, `artifact/*`, `system df/prune`, `store dump`
- Leave these out of scope for this pass:
  - global `ExecutionMode` / `resolve_mode`
  - `model`, `exec`, `llm chat`, `llm prompt`
  - `/boards/runs`
  - changing `POST /runs/{id}/start` to spawn a worker
  - introducing worker-RPC execution

### 2. Rename and shrink the hidden worker entrypoint
- Rename the hidden internal subcommand from `__detached` to `__runner`.
- Rename the enum variant, parser tests, help snapshots, and the command module/test names so internal terminology matches the future worker model.
- Narrow the hidden command contract to the minimum needed for its current thin behavior:
  - keep `run_id`
  - keep `resume`
  - keep storage selection via globals/settings
  - remove `run_dir`
  - remove `launcher_path`
- Keep its implementation simple for now:
  - connect to the server
  - call `POST /runs/{id}/start`
  - poll terminal status
- Do not keep `__detached` as an alias. This pass is the rename.

### 3. Remove launcher-era compatibility
- Delete the launcher-record subsystem in `commands/run/launcher.rs`:
  - record file creation/removal
  - stale PID cleanup
  - process-command matching
  - run-id fallback via launcher metadata
- Simplify `attach` to a server-only model when no explicit child handle is supplied:
  - infer run ID from the explicit argument or `run_dir/id.txt`
  - infer storage dir from the `runs/<id>` path shape only
  - cancel via `POST /runs/{id}/cancel`
  - remove launcher-PID probing, launcher-based kill logic, and “server-owned vs launcher-owned” branching
- Remove the now-dead `engine_child` compatibility path from `attach` if it is no longer used by production callers.
- Update `run`, `resume`, and related comments/tests so they describe server-owned execution accurately instead of launcher/detach-era behavior.

### 4. Collapse duplicated server-backed run resolution
- Add one small internal helper layer for server-backed run lookup and state access.
- Move the repeated pattern out of individual commands:
  - connect to server for storage dir
  - list durable run summaries
  - resolve a user-supplied run selector against `runs_base(...)`
  - optionally fetch current state/events
- Repoint already-server-backed commands to this helper so they stop hand-rolling the same summary lookup logic.
- Keep behavior unchanged; this is compaction, not a semantic migration.

### 5. Preserve the single-store ownership invariant explicitly
- Do not add any direct SlateDB access to `__runner`.
- Treat the server as the only SlateDB reader/writer.
- Keep the future `__runner` model aligned with server-owned storage:
  - the worker process may execute workflow logic
  - all durable run mutation must still go through server HTTP endpoints over the Unix socket
- In this cleanup pass, reflect that invariant in naming, comments, and any internal helper boundaries so later worker-RPC work does not have to undo new local-store assumptions.

### 6. Remove stale transitional tests and comments
- Rewrite or delete tests that only exist to preserve launcher-record or `__detached` behavior.
- Keep tests that prove the intended architecture:
  - server-backed detached run creation/start/attach
  - no launcher record is created in normal run/start/resume flow
  - hidden `__runner` still parses and works with its reduced contract
- Update misleading comments and docstrings that still describe local detached engine ownership.

## Test Plan
- Targeted CLI tests:
  - `cmd::start::*`
  - `cmd::attach::*`
  - `cmd::detached::*` renamed to `cmd::runner::*`
  - `cmd::resume::*`
  - `cmd::logs::*`
  - `cmd::wait::*`
- Add or update focused tests for:
  - attach infers run ID from `id.txt` without launcher metadata
  - ctrl-c / detach uses server cancel for active runs
  - `__runner` help/parser no longer mentions launcher-era flags
  - no launcher files are created anywhere in the normal run/start/resume flow
- Run full verification:
  - `cargo nextest run -p fabro-cli --no-fail-fast`
  - `cargo nextest run -p fabro-server`
  - `cargo fmt --check --all`
  - `cargo clippy --workspace -- -D warnings`

## Assumptions And Defaults
- `/boards/runs` is demo-only and ignored in this pass.
- `POST /runs/{id}/start` continues to queue and execute runs inside the server process for now.
- The future worker model is ordinary server-owned child processes, not detached launcher-managed processes.
- The server remains the single SlateDB reader/writer; future `__runner` processes must use server HTTP endpoints over the Unix socket for durable mutation.
- `__runner` is retained now because a later plan will make it the server-launched per-run worker, but that worker flip is explicitly not part of this cleanup pass.
- The local run directory still exists and remains a valid source for `id.txt` and runtime/artifact paths where the current code already depends on it.
- Broader standalone-to-server cleanup outside the proven run lifecycle area is deferred to a later plan.
