All 1655 unit tests pass. 179 integration tests pass. No failures.

Let me do one final summary of all the changes made:

## Summary of Changes

### 1. `lib/crates/fabro-workflows/src/hook/types.rs`
- Added `Self::SandboxReady` to `is_blocking_by_default()` match arm — `SandboxReady` is now blocking by default
- Updated test `hook_event_blocking_defaults` to verify `SandboxReady` is blocking and `SandboxCleanup` is not

### 2. `lib/crates/fabro-workflows/src/event.rs`
- Added `WorkflowRunEvent::SandboxInitialized { working_directory: String }` variant
- Added `trace()` implementation for the new variant (logs via `info!`)
- Added serialization/deserialization test
- Added `flatten_event` test

### 3. `lib/crates/fabro-workflows/src/engine.rs`
- Added `setup_remote_git()` public function (moved from `cli/run.rs`)
- Added `LifecycleConfig` struct with `setup_commands`, `setup_command_timeout_ms`, and `devcontainer_phases`
- Added `run_with_lifecycle()` method that:
  1. Calls `sandbox.initialize()`
  2. Fires `SandboxReady` hook (blocking — can abort run)
  3. Emits `SandboxInitialized` event
  4. Runs remote git setup if sandbox is remote and config doesn't already have git info
  5. Runs setup commands inside sandbox
  6. Runs devcontainer lifecycle phases inside sandbox
  7. Calls `run_internal()` (or checkpoint resume path)
- Added `cleanup_sandbox()` method that fires `SandboxCleanup` hook and conditionally cleans up
- Added 5 tests for the new methods:
  - `run_with_lifecycle_fires_sandbox_initialized_event`
  - `run_with_lifecycle_runs_setup_commands`
  - `run_with_lifecycle_setup_failure_aborts_run`
  - `cleanup_sandbox_fires_hook`
  - `run_with_lifecycle_emits_events_in_order`

### 4. `lib/crates/fabro-workflows/src/cli/run.rs`
- **`run_command`**: 
  - Moved `ReadBeforeWriteSandbox` wrapping before engine construction
  - Removed inline `sandbox.initialize()`, remote git setup, setup commands, devcontainer lifecycle
  - Added `SandboxInitialized` event listener (registered before emitter Arc wrapping) that writes `sandbox.json` and updates progress UI
  - Added SSH access listener
  - Builds `LifecycleConfig` and calls `engine.run_with_lifecycle()` instead of `engine.run()`
  - Replaced inline cleanup with `engine.cleanup_sandbox()`
  - Added `FinalizeConfig` struct to capture config fields needed after `run_with_lifecycle` consumes config
- **`run_from_branch`**: 
  - Moved `ReadBeforeWriteSandbox` wrapping before engine construction
  - Remote sandbox init + branch checkout converted to setup_commands in `LifecycleConfig`
  - Uses `run_with_lifecycle()` instead of `run_from_checkpoint()`
  - Added `engine.cleanup_sandbox()` call (previously just did `sandbox.cleanup()`)
  - Added `FinalizeConfig` for post-run operations
- Removed `setup_remote_git()` function (moved to engine.rs)
- Added `FinalizeConfig` struct and `write_finalize_commit_from()` function

### 5. `docs/agents/hooks.mdx`
- Updated `sandbox_ready` to show "Yes" for blocking by default
- Updated description to "After the sandbox is initialized and ready"
- Added `sandbox_ready` to the "Blocking by default" list