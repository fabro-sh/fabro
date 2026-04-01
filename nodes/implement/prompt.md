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


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan. Use red/green TDD.