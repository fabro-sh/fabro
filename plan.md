# Plan: Use short hex IDs for subagents instead of UUIDs

## Summary

GitHub issue #126: Subagent IDs are currently full UUID v4 strings (36 chars, e.g. `550e8400-e29b-41d4-a716-446655440000`). These are verbose in CLI output and error-prone when referenced by the LLM in tools like `send_input`, `wait`, and `close_agent`. The codebase already truncates agent IDs to 8 chars for display in multiple places. This change replaces UUID generation with a random 8-char hex string (e.g. `a3f1b20c`) so the full ID matches what's already displayed.

## Files to modify

### 1. `lib/crates/fabro-agent/Cargo.toml`
- Add `rand.workspace = true` to `[dependencies]`. (`rand = "0.8"` is already defined in workspace root `Cargo.toml`; other crates like `fabro-cli`, `fabro-llm`, `fabro-workflows` already use it.)
- Do **not** remove `uuid.workspace = true` — it's still used in `session.rs:58` for session IDs.

### 2. `lib/crates/fabro-agent/src/subagent.rs`
- **Line 67**: Replace `uuid::Uuid::new_v4().to_string()` with `format!("{:08x}", rand::random::<u32>())`.
- This generates 8-char lowercase hex strings with ~4 billion possible values — collision-free within a session.

### 3. `lib/crates/fabro-agent/src/cli.rs`
Remove the `short_id` truncation pattern in 5 places. Since IDs are now 8 chars, `short_id == agent_id`, so use `agent_id` directly:

- **Line 542** (`SubAgentSpawned` handler): Remove `let short_id = &agent_id[..8.min(agent_id.len())];` and replace `{short_id}` with `{agent_id}` in the format string.
- **Line 561** (`SubAgentCompleted` handler): Same removal and replacement.
- **Line 574** (`SubAgentFailed` handler): Same removal and replacement.
- **Line 583** (`SubAgentClosed` handler): Same removal and replacement.
- **Line 596** (`SubAgentEvent` handler, verbose mode): Same removal and replacement.

### 4. `lib/crates/fabro-cli/src/commands/run_progress.rs`
Remove the `short_id` truncation pattern in 2 places:

- **Line 1416** (`SubAgentSpawned` handler): Remove `let short_id = &agent_id[..agent_id.len().min(8)];` and replace `{short_id}` with `{agent_id}` in the format string.
- **Line 1432** (`SubAgentCompleted` handler): Same removal and replacement.

## Step-by-step implementation

1. **Add `rand` dependency to `fabro-agent`**:
   In `lib/crates/fabro-agent/Cargo.toml`, add `rand.workspace = true` to the `[dependencies]` section (e.g. after the `uuid.workspace = true` line).

2. **Replace UUID generation in `subagent.rs`**:
   In `lib/crates/fabro-agent/src/subagent.rs` line 67, change:
   ```rust
   let agent_id = uuid::Uuid::new_v4().to_string();
   ```
   to:
   ```rust
   let agent_id = format!("{:08x}", rand::random::<u32>());
   ```

3. **Remove `short_id` truncation in `cli.rs`**:
   In `lib/crates/fabro-agent/src/cli.rs`, for each of the 5 occurrences of `let short_id = &agent_id[..8.min(agent_id.len())];` (lines 542, 561, 574, 583, 596):
   - Delete the `let short_id = ...` line.
   - Replace `{short_id}` with `{agent_id}` in the corresponding format string on the same match arm.

4. **Remove `short_id` truncation in `run_progress.rs`**:
   In `lib/crates/fabro-cli/src/commands/run_progress.rs`, for each of the 2 occurrences of `let short_id = &agent_id[..agent_id.len().min(8)];` (lines 1416, 1432):
   - Delete the `let short_id = ...` line.
   - Replace `{short_id}` with `{agent_id}` in the corresponding format string.

## Verification

- `cargo build --workspace` — clean build with no errors.
- `cargo test -p fabro-agent` — all existing subagent tests pass. Tests use hardcoded IDs like `"sa-1"`, not UUIDs, so no test changes needed.
- `cargo clippy --workspace -- -D warnings` — no new warnings.
- `cargo fmt --check --all` — formatting is clean.

## Test cases

No new test cases are needed. The existing tests in `subagent.rs` (e.g. `spawn_creates_agent_and_returns_id`) already verify that:
- `agent_id` is non-empty
- The agent can be looked up by its ID
- Spawn/wait/close/send_input work with the generated IDs

The generated IDs will now be 8 chars instead of 36, but the tests don't assert on length or format, so they pass unchanged.
