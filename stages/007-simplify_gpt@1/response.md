Completed the simplify/review cleanup.

Notes:
- I launched the 3 requested review agents twice, but the subagent backend failed during result retrieval both times with an OpenAI “item not found” tool error. I closed the failed agents and continued the review directly against the diff/codebase.

Cleanups applied:
- `fabro-sandbox/src/local.rs`
  - Deduplicated stdout/stderr drain logic into a shared `drain_pipe` helper.
  - Preserved stream-specific warn messages and structured `stream` / `error` fields.
  - Added a unit test for the read-error path using a custom failing `AsyncRead`.
- `fabro-workflow/src/handler/parallel.rs`
  - Changed `parallel_base_checkpoint_failed` to use `notice_with_tail(...)` with `default_redacted_output_tail(...)` instead of embedding the full `display_for_log(...)` rendering in the notice message.
- `fabro-workflow/src/pipeline/initialize.rs`
  - Cached `sandbox.origin_url().is_some()` as `sandbox_has_origin` so the setup-git fallback gate is clearer and not recomputed.
- `fabro-workflow/src/handler/prompt.rs`
  - Simplified test code to use the already-imported `Emitter`.

Verification run:
- `cargo +nightly-2026-04-14 fmt --all`
- `cargo +nightly-2026-04-14 fmt --check --all`
- `cargo test -p fabro-sandbox drain_pipe_returns_empty_buffer_after_read_failure`
- `cargo test -p fabro-workflow initialize_emits_worktree_skipped_no_git_in_non_git_cwd`
- `cargo test -p fabro-workflow prompt_handler_dispatches_to_backend_one_shot`
- `cargo +nightly-2026-04-14 clippy -p fabro-sandbox -p fabro-workflow --all-targets -- -D warnings`

All passing.