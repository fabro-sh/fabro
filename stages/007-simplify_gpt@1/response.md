Implemented the cleanup review fixes.

Note: I launched the three requested review subagents twice, but the subagent tool failed both times with the same infrastructure persistence error. I continued with the equivalent direct review against the full branch diff.

Changes made:
- `lib/crates/fabro-store/src/run_state.rs`
  - Added run-projection handling for `agent.cli.completed`, `agent.cli.cancelled`, and `agent.cli.timed_out`.
  - Preserves stdout/stderr, termination, and timing metadata for agent CLI terminal events.
  - Added focused projection tests for all three agent CLI terminal cases.
- `lib/crates/fabro-workflow/src/handler/llm/api.rs`
  - Fixed API backend cancellation post-checks around `create_session` / `create_session_for`, including error-return paths.
  - Changed non-LLM terminal agent session failures to `Error::Precondition`, matching the implementation plan’s intended non-retryable behavior.
- `lib/crates/fabro-workflow/src/handler/llm/cli.rs`
  - Removed duplicate local `shell_quote` helper and reused the existing exported helper.
  - Quoted CLI prompt/model/path-related shell interpolations.
- `lib/crates/fabro-workflow/src/handler/parallel.rs`
  - Aborts remaining branch tasks when a branch propagates `Error::Cancelled`, avoiding detached stale work.

Verification passed:
- `cargo test -p fabro-store run_state --lib`
- `cargo test -p fabro-workflow handler::llm::api --lib`
- `cargo test -p fabro-workflow handler::llm::cli --lib`
- `cargo check -q --workspace`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo +nightly-2026-04-14 clippy -q --workspace --all-targets -- -D warnings`
- `git diff --check`

No OpenAPI schema changes were made, so API/client regeneration was unnecessary.