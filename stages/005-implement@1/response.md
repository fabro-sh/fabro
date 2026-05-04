Here is a summary of all the changes made:

## Summary of Changes

### Core Type Migration: `Arc<AtomicBool>` → `CancellationToken`

**fabro-core** (2 files):
- `executor.rs`: `ExecutorOptions.cancel_token` changed from `Option<Arc<AtomicBool>>` to `Option<CancellationToken>`. The between-node cancellation check uses `token.is_cancelled()` instead of `token.load(Ordering::Relaxed)`. `ExecutorBuilder::cancel_token()` takes `CancellationToken`.
- `stall.rs`: `StallWatchdog` field renamed from `cancel_token: Arc<AtomicBool>` to `stall_token: CancellationToken`. Uses `stall_token.cancel()` instead of `cancel_token.store(true, ...)`.

**fabro-workflow** (25 files):
- `run_options.rs`: `RunOptions.cancel_token` changed from `Option<Arc<AtomicBool>>` to non-optional `CancellationToken`.
- `services.rs`: `RunServices.cancel_requested: Option<Arc<AtomicBool>>` → `cancel_token: CancellationToken`. Added `cancel_token()` accessor. Renamed `with_cancel_requested()` to `with_cancel_token()`. Removed `sandbox_cancel_token()` bridge function (the 10ms polling bridge).
- `pipeline/execute.rs`: Always passes `cancel_token` to `ExecutorBuilder` (no more `Option` wrapping).
- `pipeline/initialize.rs`: Uses `cancel_token.child_token()` directly for setup commands instead of the bridge.
- `devcontainer_bridge.rs`: Takes `CancellationToken` directly, uses `child_token()` for per-command cancellation.
- `handler/command.rs`: Uses `services.run.cancel_token().child_token()` for sandbox streaming.
- `handler/human.rs`: Uses `services.run.cancel_token().is_cancelled()`.
- `handler/manager_loop.rs`: Uses `services.run.cancel_token().child_token()` for child workflows. Parent cancellation propagates to children via `child_token()`. Stop condition and max-cycles use `child_run_token.cancel()`.
- `operations/start.rs`: `StartServices.cancel_token` and `RunSession.cancel_token` changed to non-optional `CancellationToken`. Guard drop implementations use `is_cancelled()`.

**fabro-server** (3 files):
- `server.rs`: `ManagedRun.cancel_token` changed to `Option<CancellationToken>`. Uses `token.cancel()` and `is_cancelled()`.
- `handler/lifecycle.rs`: Uses `token.cancel()`.
- `Cargo.toml`: Added `tokio-util` dependency.

**fabro-cli** (2 files):
- `runner.rs`: Signal handlers and worker control stream use `CancellationToken`.

### Sandbox Streaming Timeout: `u64` → `Option<u64>`

**fabro-sandbox** (5 files):
- `sandbox.rs`: `exec_command_streaming` signature changed `timeout_ms: u64` → `timeout_ms: Option<u64>`. Default implementation uses `u64::MAX` fallback.
- `local.rs`: Uses pending future pattern for `None` timeout.
- `docker.rs`: Uses pending future pattern for `None` timeout.
- `daytona/mod.rs`: `wait_for_completion` takes `Option<Duration>`, uses pending future pattern.
- `worktree.rs`: Forwards `Option<u64>`.

### CodergenBackend Cancellation

- `CodergenBackend::run` signature now takes `cancel_token: CancellationToken`.
- `AgentHandler::execute`: Explicit `Error::Cancelled` propagation before `to_fail_outcome()`.
- `PromptHandler`: Same explicit `Error::Cancelled` propagation.
- `ParallelHandler`: `Error::Cancelled` from branches propagates to parent instead of being aggregated as failed branch outcome.
- All 19 `CodergenBackend` implementations updated (API, CLI, BackendRouter, test stubs).

### New Event Types

**fabro-types** (2 files):
- Added `AgentCliCancelledProps` and `AgentCliTimedOutProps` (with `stdout`, `stderr`, `duration_ms`).
- Added `EventBody::AgentCliCancelled` and `EventBody::AgentCliTimedOut` variants.

**fabro-workflow** (4 files):
- Added `Event::AgentCliCancelled` and `Event::AgentCliTimedOut` variants.
- Added event name mappings (`agent.cli.cancelled`, `agent.cli.timed_out`).
- Added event-to-body conversion.
- Added stored_fields mapping (stores `node_id` in envelope).