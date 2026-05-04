All clean. The implementation is complete. Let me summarize what was implemented:

## Summary

Implemented the full plan in `docs/plans/2026-05-03-fix-agent-stage-cancellation-plan.md`:

### Core changes
- **`fabro-core::executor`**: `ExecutorOptions.cancel_token` migrated from `Arc<AtomicBool>` to `Option<CancellationToken>`. `is_cancelled()` replaces `load(Ordering::Relaxed)`.
- **`fabro-core::stall`**: `StallWatchdog` migrated to `CancellationToken` for stall_token.
- **`fabro-workflow::run_options::RunOptions`**: `cancel_token` is now non-optional `CancellationToken`.
- **`fabro-workflow::services::RunServices`**: replaced `cancel_requested: Option<Arc<AtomicBool>>` with `cancel_token: CancellationToken`. Added `cancel_token()` getter; `with_cancel_token(...)` is `pub(crate)`. Removed the `sandbox_cancel_token()` 10ms polling bridge.

### Sandbox streaming timeout
- `Sandbox::exec_command_streaming` now takes `timeout_ms: Option<u64>`. `None` means unbounded; `Some(ms)` means time out. All implementations updated (local, docker, daytona, worktree, default trait fallback uses `u64::MAX`). Production paths use a `pending`-based timeout future, not `Duration::from_millis(u64::MAX)`.

### Pipeline & handlers
- **`pipeline/initialize.rs`** and **`devcontainer_bridge.rs`**: pass `child_token()` from the run cancel token into sandbox commands.
- **`handler/command.rs`**: passes `Some(node.timeout().map_or(600_000, ...))` and `services.run.cancel_token().child_token()`.
- **`handler/manager_loop.rs`**: child run gets `services.run.cancel_token().child_token()`. Stop-condition and max-cycles call `child_run_token.cancel()`. Parent → child propagation via `child_token()`; child cancellation does not cancel parent.
- **`handler/human.rs`**: uses `services.run.cancel_token().is_cancelled()`.

### Agent backends (CodergenBackend trait)
- `CodergenBackend::run` now takes `cancel_token: CancellationToken`.
- `AgentHandler::execute`, `PromptHandler::execute`, and `ParallelHandler` all add explicit `Err(Error::Cancelled) => return Err(Error::Cancelled)` arms before retryable/fail-outcome conversion.

### CLI agent backend rework (`handler/llm/cli.rs`)
- Removed `setsid &` background launch + polling + `cat stdout/stderr file` pattern.
- Now uses `sandbox.exec_command_streaming(..., timeout, ..., Some(cancel_token.child_token()), output_callback)`.
- `node.timeout()` maps to `Some(ms)` or `None` (preserves unbounded CLI runtime).
- Emits `agent.cli.cancelled` on `Cancelled`, `agent.cli.timed_out` on `TimedOut`, `agent.cli.completed` only on `Exited`. Returns `Error::Cancelled` on cancellation; handler timeout error on timeout.
- `ensure_cli`, login command pass child tokens through.

### API agent backend (`handler/llm/api.rs`)
- Added `SessionCancelBridgeGuard` (replace/abort/Drop) that bridges run-token cancellation to `Session::cancel_token()` and `interrupt_reason_handle()`.
- Added `AgentApiErrorDisposition` + `classify_agent_error(err, allow_failover)` with three states: `Cancelled`, `FailoverEligible(fabro_llm::Error)`, `Terminal(Error)`.
- Pre/post-cancel checks around `Client::from_source` calls.
- `initialize()` errors classified the same way as `process_input()` errors, with failover support.
- Cached sessions never reinserted on cancellation/failure; `bridge.abort()` runs before reinsertion.

### `Session::initialize`
- Signature changed to `Result<(), fabro_agent::Error>`.
- Helpers `resolve_sandbox_mcp_servers`, `start_sandbox_mcp_server`, `build_env_context`, `discover_memory`, `discover_skills` take `&CancellationToken` and surface `Interrupted(Cancelled)`. Sandbox MCP startup terminates the MCP process group on cancellation via `kill_mcp_pid`.
- All call sites updated (`fabro-agent::cli`, `subagent`, `v4a_patch`, `fabro-retro::retro_agent`, README, SDK docs).

### New events
- `Event::AgentCliCancelled` (external `agent.cli.cancelled`) and `Event::AgentCliTimedOut` (external `agent.cli.timed_out`) added across `fabro-types`, `fabro-workflow` (events, names, stored fields, convert), and `fabro-workflow::operations::fork`. Conversion tests added.

### Server / CLI entry points
- `StartServices.cancel_token` and `RunSession.cancel_token` are `CancellationToken`.
- `ManagedRun.cancel_token` is `Option<CancellationToken>`.
- `DetachedRunBootstrapGuard`/`DetachedRunCompletionGuard` `Drop` reads `is_cancelled()` (no longer triggers cancellation).
- CLI runner signal handlers (`install_signal_handlers`), worker control stream all use `CancellationToken`.

### Test additions
- New executor test `executor_cancel_token_fired_during_run_returns_cancelled`.
- AgentCliBackend cancellation/timeout tests (`agent_cli_backend_run_emits_cancelled_event_*`, `agent_cli_backend_run_emits_timed_out_event_*`).
- AgentApi bridge guard + `classify_agent_error` unit tests (10 new tests by subagent).
- Convert tests for new event variants.

### Verification
- `cargo build --workspace` ✅
- `cargo +nightly-2026-04-14 fmt --check --all` ✅
- `cargo +nightly-2026-04-14 clippy --workspace --all-targets -- -D warnings` ✅
- **3131 tests passing across `fabro-workflow`, `fabro-agent`, `fabro-types`, `fabro-sandbox`, `fabro-core`, `fabro-server`, `fabro-cli`** (162 skipped, 0 failed).