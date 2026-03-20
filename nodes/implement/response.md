Both changes look correct:

1. **Inside `execute_with_retry`** (line 1080): `StageStarted` is now emitted at the top of the `for attempt in 1..=policy.max_attempts` loop, using the loop variable `attempt` for the correct attempt number.

2. **In `run_inner`** (line 1862): The old `StageStarted` emission that was before the loop has been removed. The `StageStart` hook remains outside the loop as specified.

Let me do a final summary of the changes:

## Summary

**Problem:** When a stage fails with a transient error and is retried, the CLI progress UI freezes because `StageFailed` removes the stage from `active_stages`, but no `StageStarted` event is emitted for the retry attempt, so subsequent agent events silently drop.

**Fix:** Two edits in `lib/crates/fabro-workflows/src/engine.rs`:

1. **Added `StageStarted` emission inside the retry loop** (in `execute_with_retry`): Moved the event emission to the top of the `for attempt in 1..=policy.max_attempts` loop so every attempt — including retries — emits `StageStarted` with the correct `attempt` number.

2. **Removed the old `StageStarted` emission before the loop** (in `run_inner`): The pre-loop emission with hardcoded `attempt: 1` was removed since it's now handled inside the retry loop. The `StageStart` hook remains outside the loop (hooks shouldn't re-fire on retries).

**New test:** `retry_emits_stage_started_per_attempt` — Creates a handler (`FailOnceThenSucceedHandler`) that returns a retryable error on the first call and succeeds on the second, then verifies that `StageStarted` is emitted twice with `attempt: 1` and `attempt: 2`.

**Verification:**
- ✅ `cargo test -p fabro-workflows --lib` — all 867 tests pass
- ✅ `cargo clippy --workspace -- -D warnings` — no warnings