Goal: # Add storage directory check to `fabro doctor`

## Context

`fabro doctor` validates the installation but doesn't check that the storage directory (where runs, store data, etc. live) exists and is usable. Adding this check surfaces misconfiguration early — e.g. a `storage_dir` override pointing to a nonexistent or read-only path.

## Plan

### 1. Add `check_storage_dir` pure function in `doctor.rs`

**File:** `lib/crates/fabro-cli/src/commands/doctor.rs`

Add a function like the existing `check_config`:

```rust
fn check_storage_dir(path: &Path, readable: bool, writable: bool) -> CheckResult
```

- **Summary always shows the resolved path** (e.g. `/Users/you/.fabro`) — same pattern as `check_config` which puts the path in `summary`.
- **Pass** — dir exists, readable, writable.
- **Error** — dir doesn't exist, or not readable, or not writable. Remediation: create it or fix permissions.
- Details (verbose): existence, read, write status as individual lines.

### 2. Gather state in `run_doctor`

Before the pure-checks section, resolve the storage dir and probe it:

```rust
let storage_dir = cli_settings.storage_dir();
let exists = storage_dir.is_dir();
let readable = std::fs::read_dir(&storage_dir).is_ok();
let writable = tempfile::tempfile_in(&storage_dir).is_ok();  // or write+remove a temp file
```

Use `std::fs` directly — no async/live probe needed for local filesystem checks.

### 3. Add to "Required" section

Insert `check_storage_dir` result into the "Required" section, after the "Configuration" check and before "LLM providers" — storage is fundamental.

### 4. Add unit tests

Follow the existing test pattern (pure function tests with synthetic inputs). Cover:
- Dir exists + readable + writable → Pass
- Dir doesn't exist → Error
- Dir exists but not writable → Error

Use a `tempdir` for real filesystem assertions in a couple of tests.

### 5. Add integration tests in `it/cmd/doctor.rs`

**File:** `lib/crates/fabro-cli/tests/it/cmd/doctor.rs` (existing, has 4 tests)

Add 2 tests using the existing `test_context!()` + `fabro_snapshot!` pattern:

- **`storage_dir_shown_in_output`** — `TestContext` already creates a temp `storage_dir` and sets `FABRO_STORAGE_DIR`. Run `doctor --dry-run`, snapshot-assert that "Storage directory" line appears with the path in the summary.
- **`storage_dir_missing_shows_error`** — Override `FABRO_STORAGE_DIR` to a nonexistent path via `.env("FABRO_STORAGE_DIR", "/tmp/nonexistent-fabro-xyz")`. Run `doctor --dry-run`, snapshot-assert that the check shows error status with the path and remediation text.

Both tests use `--dry-run` to skip live probes and `fabro_snapshot!` for inline snapshot assertions. Add a filter to normalize the temp dir path (e.g. `[STORAGE_DIR]`).

## Files to modify

- `lib/crates/fabro-cli/src/commands/doctor.rs` — new check function + gather state + wire into section + unit tests
- `lib/crates/fabro-cli/tests/it/cmd/doctor.rs` — 2 new integration tests

## Verification

- `cargo nextest run -p fabro-cli -- doctor` — unit + integration tests pass
- `cargo clippy --workspace -- -D warnings` — no lint issues
- `fabro doctor` — shows new "Storage directory" check with the resolved path
- `fabro doctor -v` — shows detail lines for existence/read/write
- `FABRO_STORAGE_DIR=/nonexistent fabro doctor` — shows error for missing dir


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check -q --workspace 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1`
  - Stdout: (empty)
  - Stderr: (empty)
- **implement**: success
  - Model: claude-opus-4-6, 74.4k tokens in / 10.7k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/doctor.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/it/cmd/doctor.rs
- **simplify_opus**: success
  - Model: claude-opus-4-6, 48.3k tokens in / 13.2k out
  - Files: /home/daytona/workspace/lib/crates/fabro-cli/src/commands/doctor.rs, /home/daytona/workspace/lib/crates/fabro-cli/tests/it/cmd/doctor.rs
- **simplify_gpt**: success
  - Model: gpt-5.4, 2.5m tokens in / 26.5k out
- **verify**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1 && cargo nextest run --cargo-quiet --workspace --status-level fail 2>&1`
  - Stdout:
    ```
    (53 lines omitted)
        {"id":"019d4946-b0cf-7961-aa4b-f5d1398ee15f","ts":"2026-04-01T13:41:02.799Z","run_id":"01KN4MDC26TQ8E0SFD7GYZ1GEH","event":"run.completed","properties":{"duration_ms":86,"artifact_count":0,"status":"success"}}
        {"id":"019d4946-b0e7-7c31-9dee-e16ce7adab95","ts":"2026-04-01T13:41:02.823Z","run_id":"01KN4MDC26TQ8E0SFD7GYZ1GEH","event":"sandbox.cleanup.started","properties":{"provider":"local"}}
    
        thread 'cmd::logs::logs_follow_detached_run_streams_until_completion' (42483) panicked at /root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/insta-1.46.3/src/runtime.rs:719:13:
        snapshot assertion for 'logs_follow_detached_run_streams_until_completion' failed in line 173
        note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
    
      Cancelling due to test failure: 3 tests still running
    [>  5.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    [> 10.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    [> 15.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
     TERMINATING [> 20.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
         TIMEOUT [  20.005s] ( 503/3474) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
      stdout ───
    
        running 1 test
    
        (test timed out)
    
    ────────────
         Summary [  21.873s] 503/3474 tests run: 501 passed, 1 failed, 1 timed out, 180 skipped
            FAIL [   0.328s] ( 500/3474) fabro-cli::it cmd::logs::logs_follow_detached_run_streams_until_completion
         TIMEOUT [  20.005s] ( 503/3474) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    warning: 2971/3474 tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)
    error: test run failed
    ```
  - Stderr: (empty)
- **fixup**: success
  - Model: claude-opus-4-6, 8.3k tokens in / 1.5k out
  - Files: /home/daytona/workspace/lib/crates/fabro-server/tests/it/api.rs
- **verify**: fail
  - Script: `cargo clippy -q --workspace -- -D warnings 2>&1 && cargo nextest run --cargo-quiet --workspace --status-level fail 2>&1`
  - Stdout:
    ```
    (53 lines omitted)
        {"id":"019d4946-b0cf-7961-aa4b-f5d1398ee15f","ts":"2026-04-01T13:41:02.799Z","run_id":"01KN4MDC26TQ8E0SFD7GYZ1GEH","event":"run.completed","properties":{"duration_ms":86,"artifact_count":0,"status":"success"}}
        {"id":"019d4946-b0e7-7c31-9dee-e16ce7adab95","ts":"2026-04-01T13:41:02.823Z","run_id":"01KN4MDC26TQ8E0SFD7GYZ1GEH","event":"sandbox.cleanup.started","properties":{"provider":"local"}}
    
        thread 'cmd::logs::logs_follow_detached_run_streams_until_completion' (42483) panicked at /root/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/insta-1.46.3/src/runtime.rs:719:13:
        snapshot assertion for 'logs_follow_detached_run_streams_until_completion' failed in line 173
        note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
    
      Cancelling due to test failure: 3 tests still running
    [>  5.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    [> 10.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    [> 15.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
     TERMINATING [> 20.000s] (─────────) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
         TIMEOUT [  20.005s] ( 503/3474) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
      stdout ───
    
        running 1 test
    
        (test timed out)
    
    ────────────
         Summary [  21.873s] 503/3474 tests run: 501 passed, 1 failed, 1 timed out, 180 skipped
            FAIL [   0.328s] ( 500/3474) fabro-cli::it cmd::logs::logs_follow_detached_run_streams_until_completion
         TIMEOUT [  20.005s] ( 503/3474) fabro-cli::it cmd::attach::attach_before_completion_streams_to_finished_state
    warning: 2971/3474 tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)
    error: test run failed
    ```
  - Stderr: (empty)

## Context
- failure_class: canceled
- failure_signature: verify|canceled|script failed with exit code: <n> ## stdout ──────────── nextest run id <hex>-07d6-4fe9-be74-<hex> with nextest profile: default starting <n> tests across <n> binaries (<n> tests skipped) fail [ <n>.328s] ( <n>/<n>) 


The verify step failed. Read the build output from context and fix all clippy lint warnings and test failures.